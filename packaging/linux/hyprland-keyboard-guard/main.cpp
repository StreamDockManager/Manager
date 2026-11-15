// Optional, explicitly loaded compositor-side physical-input authority
// No injection, privileged input devices, background recording, or key logging
#include <algorithm>
#include <array>
#include <cerrno>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <fcntl.h>
#include <fstream>
#include <hyprland/src/Compositor.hpp>
#include <hyprland/src/desktop/state/FocusState.hpp>
#include <hyprland/src/desktop/view/Window.hpp>
#include <hyprland/src/devices/IKeyboard.hpp>
#include <hyprland/src/event/EventBus.hpp>
#include <hyprland/src/managers/SeatManager.hpp>
#include <hyprland/src/managers/SessionLockManager.hpp>
#include <hyprland/src/managers/input/InputManager.hpp>
#include <hyprland/src/plugins/PluginAPI.hpp>
#include <hyprland/src/protocols/InputMethodV2.hpp>
#include <memory>
#include <nlohmann/json.hpp>
#include <set>
#include <stdexcept>
#include <string>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>
#include <vector>
#include <wayland-server-core.h>

namespace {
using Json = nlohmann::json;
using Clock = std::chrono::steady_clock;
constexpr auto SOURCE = "efb50993780079460b0cbed1363e2166a2de1d9f";
constexpr size_t MAX_FRAME = 65536, MAX_EVENTS = 1024, MAX_CLIENTS = 4;
// Reject compositor states where physical input must not be observed
bool protectedSession() {
  const auto ime =
      g_pInputManager ? g_pInputManager->m_relay.m_inputMethod.lock() : nullptr;
  return !g_pCompositor || !g_pSessionLockManager || !g_pSeatManager ||
         !g_pInputManager || !g_pCompositor->m_sessionActive ||
         !g_pCompositor->m_dpmsStateOn || g_pCompositor->m_isShuttingDown ||
         g_pSessionLockManager->isSessionLocked() ||
         g_pSessionLockManager->anySessionLockSurfacesPresent() ||
         bool(g_pSeatManager->m_seatGrab) || (ime && ime->hasGrab());
}
// Resolve a peer executable through its current process link
std::string executable(pid_t pid) {
  std::array<char, 4096> buf{};
  const auto n = readlink(("/proc/" + std::to_string(pid) + "/exe").c_str(),
                          buf.data(), buf.size());
  return n > 0 && size_t(n) < buf.size() ? std::string(buf.data(), n) : "";
}
// Require every process UID to match the current non-root user
bool ordinaryProcess(pid_t pid) {
  if (pid <= 0)
    return false;
  std::ifstream file("/proc/" + std::to_string(pid) + "/status");
  std::string line;
  while (std::getline(file, line))
    if (line.starts_with("Uid:")) {
      unsigned real, effective, saved, fs;
      return sscanf(line.c_str(), "Uid: %u %u %u %u", &real, &effective, &saved,
                    &fs) == 4 &&
             real == getuid() && effective == getuid() && saved == getuid() &&
             fs == getuid() && real != 0;
    }
  return false;
}
struct Client;
std::vector<std::unique_ptr<Client>> clients;
std::string socketPath;
int listenerFd = -1;
wl_event_source *listenerSource = nullptr, *timerSource = nullptr;
struct Client {
  int fd;
  pid_t pid;
  wl_event_source *source = nullptr;
  std::string in, out, mode;
  Clock::time_point touched = Clock::now(), started = touched;
  bool dead = false, revoked = false, interference = false, armed = false;
  uint64_t sequence = 0, globalEdges = 0, knownEdges = 0;
  PHLWINDOW target;
  SP<CWLSurfaceResource> surface;
  std::vector<SP<IKeyboard>> keyboards;
  std::vector<CHyprSignalListener> signals;
  Json events = Json::array();
  // Own the accepted connection and its authenticated process identity
  Client(int fd_, pid_t pid_) : fd(fd_), pid(pid_) {}
  // Detach input listeners and release the connection resources
  ~Client() {
    detach();
    if (source)
      wl_event_source_remove(source);
    close(fd);
  }
  // Release keyboard subscriptions and retained keyboard references
  void detach() {
    signals.clear();
    keyboards.clear();
  }
  // Require a mapped unblocked native target owned by an ordinary process
  bool targetAlive() {
    return target && target->m_isMapped && !target->m_isX11 &&
           !target->isInputBlocked() && ordinaryProcess(target->getPID());
  }
  // Check window, surface and seat focus against the selected target
  bool targetFocused() {
    return targetAlive() && Desktop::focusState()->window() == target &&
           Desktop::focusState()->surface() == surface &&
           g_pSeatManager->m_state.keyboardFocus.lock() == surface;
  }
  // Revoke leases when identity, focus, session protection or keyboard provenance changes
  void check() {
    if (protectedSession()) {
      revoked = true;
      detach();
    }
    if (!mode.empty() && executable(pid) != SDM_MANAGER_EXECUTABLE) {
      revoked = true;
      detach();
    }
    if (target && (!targetAlive() || (armed && !targetFocused())))
      revoked = true;
    if (mode == "record" || mode == "observe") {
      // Unattributed keyboard edges invalidate the physical-input provenance claim
      if (globalEdges != knownEdges)
        revoked = true;
      if (keyboards.size() != g_pInputManager->m_keyboards.size())
        revoked = true;
      else
        for (size_t i = 0; i < keyboards.size(); ++i)
          if (keyboards[i] != g_pInputManager->m_keyboards[i])
            revoked = true;
    }
    if (revoked)
      detach();
  }
  // Collect currently pressed key codes from physical keyboards
  Json held() {
    std::set<uint32_t> keys;
    for (const auto &keyboard : keyboards)
      if (!keyboard->isVirtual())
        for (uint32_t code = 0; code < 768; ++code)
          if (keyboard->getPressed(code))
            keys.insert(code);
    return keys;
  }
  // Acquire the requested bounded observation or target-focus lease
  void start(const std::string &requested) {
    if (!mode.empty() || protectedSession())
      throw std::runtime_error("operation unavailable");
    if (requested != "record" && requested != "observe" &&
        requested != "target")
      throw std::runtime_error("invalid mode");
    if (requested != "target")
      for (const auto &other : clients)
        if (other.get() != this &&
            (other->mode == "record" || other->mode == "observe"))
          throw std::runtime_error("input lease already active");
    mode = requested;
    started = Clock::now();
    if (mode == "target") {
      target = Desktop::focusState()->window();
      surface = Desktop::focusState()->surface();
      if (!targetFocused() || target->getPID() == pid)
        throw std::runtime_error(
            "select an ordinary native Wayland target outside Manager");
    } else {
      keyboards = g_pInputManager->m_keyboards;
      if (std::none_of(keyboards.begin(), keyboards.end(),
                       [](const auto &keyboard) {
                         return !keyboard->isVirtual() && keyboard->m_enabled &&
                                keyboard->m_allowed;
                       }))
        throw std::runtime_error("no enabled physical keyboard");
      for (const auto &keyboard : keyboards)
        if (keyboard->m_modifiersState.latched != 0)
          throw std::runtime_error(
              "release latched keyboard modifiers before starting");
      signals.emplace_back(Event::bus()->m_events.input.keyboard.key.listen(
          [this](auto, auto &) { ++globalEdges; }));
      for (const auto &keyboard : keyboards) {
        signals.emplace_back(keyboard->m_keyboardEvents.key.listen(
            // Buffer bounded physical-key events and revoke ambiguous or protected input
            [this, keyboard](const IKeyboard::SKeyEvent &e) {
              if (keyboard->m_enabled && keyboard->m_allowed)
                ++knownEdges;
              if (!keyboard->m_enabled || !keyboard->m_allowed) {
                revoked = true;
                return;
              }
              if (keyboard->isVirtual())
                return; // includes separately authorized portal output
              if (protectedSession()) {
                revoked = true;
                return;
              }
              if (e.state == WL_KEYBOARD_KEY_STATE_PRESSED)
                for (const auto &other : keyboards)
                  if (other != keyboard && !other->isVirtual() &&
                      other->getPressed(e.keycode))
                    revoked = true;
              if (e.keycode >= 768 || events.size() >= MAX_EVENTS) {
                revoked = true;
                return;
              }
              // Observation treats physical input as interference rather than replayable output
              if (mode == "observe")
                interference = true;
              events.push_back(Json::array(
                  {e.keycode, e.state == WL_KEYBOARD_KEY_STATE_PRESSED,
                   std::chrono::duration_cast<std::chrono::milliseconds>(
                       Clock::now() - started)
                       .count()}));
            }));
        if (mode == "observe") {
          const auto initial = keyboard->m_modifiersState;
          signals.emplace_back(keyboard->m_keyboardEvents.modifiers.listen(
              // Revoke observation when persistent modifier state changes
              [this, initial](const IKeyboard::SModifiersEvent &state) {
                if (state.latched != initial.latched ||
                    state.locked != initial.locked ||
                    state.group != initial.group)
                  revoked = true;
              }));
        }
        signals.emplace_back(keyboard->m_keyboardEvents.keymap.listen(
            [this](auto) { revoked = true; }));
        signals.emplace_back(
            keyboard->m_events.destroy.listen([this]() { revoked = true; }));
      }
    }
    signals.emplace_back(Event::bus()->m_events.input.keyboard.layout.listen(
        [this](SP<IKeyboard>, const std::string &) { revoked = true; }));
    signals.emplace_back(g_pSessionLockManager->m_events.lock.listen(
        [this]() { revoked = true; }));
    if (target) {
      signals.emplace_back(
          g_pSeatManager->m_events.keyboardFocusChange.listen([this]() {
            if (armed && !targetFocused())
              revoked = true;
          }));
      signals.emplace_back(
          Event::bus()->m_events.window.close.listen([this](PHLWINDOW w) {
            if (w == target)
              revoked = true;
          }));
    }
  }
  // Report current lease state and consume the pending event batch
  Json reply() {
    check();
    auto result = Json{{"ok", true},
                       {"protocol", 1},
                       {"source", SOURCE},
                       {"protected", protectedSession()},
                       {"revoked", revoked},
                       {"interference", interference},
                       {"seq", ++sequence},
                       {"target_alive", targetAlive()},
                       {"target_focused", targetFocused()},
                       {"label", target ? target->m_title.substr(0, 256) : ""},
                       {"held", held()},
                       {"events", events}};
    events = Json::array();
    return result;
  }
  // Validate one framed request and produce a bounded lease-state reply
  void command(const std::string &line) {
    try {
      const auto req = Json::parse(line);
      if (!req.is_object() || req.size() > 2)
        throw std::runtime_error("invalid request");
      const auto op = req.at("op").get<std::string>();
      if (op == "start")
        start(req.at("mode").get<std::string>());
      else if (op == "focus") {
        if (mode != "target" || !targetFocused())
          throw std::runtime_error("target not focused");
        armed = true;
      } else if (op != "hello" && op != "poll" && op != "alive" && op != "stop")
        throw std::runtime_error("unknown request");
      if (op != "hello" && op != "start" && mode.empty())
        throw std::runtime_error("no operation");
      auto result = reply();
      if (op == "stop") {
        detach();
        mode.clear();
        target.reset();
        surface.reset();
      }
      out = result.dump(-1, ' ', false, Json::error_handler_t::replace) + "\n";
    } catch (const std::exception &e) {
      detach();
      revoked = true;
      out = Json{{"ok", false}, {"error", e.what()}}.dump() + "\n";
    }
    if (out.size() > MAX_FRAME) {
      dead = true;
      detach();
    }
    touched = Clock::now();
  }
};
// Service nonblocking connection reads and writes without request pipelining
int clientReady(int, uint32_t mask, void *data) {
  auto &client = *static_cast<Client *>(data);
  if (mask & (WL_EVENT_HANGUP | WL_EVENT_ERROR)) {
    client.dead = true;
    client.detach();
    return 0;
  }
  if ((mask & WL_EVENT_READABLE) && client.out.empty()) {
    char buf[512];
    const auto n = recv(client.fd, buf, sizeof(buf), MSG_DONTWAIT);
    if (n == 0 || (n < 0 && errno != EAGAIN && errno != EINTR))
      client.dead = true;
    if (n > 0)
      client.in.append(buf, n);
    if (client.in.size() > 512)
      client.dead = true;
    if (auto end = client.in.find('\n'); end != std::string::npos) {
      if (end + 1 != client.in.size())
        client.dead = true; // no pipelining
      else {
        client.command(client.in.substr(0, end));
        client.in.clear();
      }
    }
  }
  if (!client.out.empty()) {
    const auto n = send(client.fd, client.out.data(), client.out.size(),
                        MSG_DONTWAIT | MSG_NOSIGNAL);
    if (n > 0)
      client.out.erase(0, n);
    else if (n < 0 && errno != EAGAIN && errno != EINTR)
      client.dead = true;
  }
  if (client.dead)
    client.detach();
  wl_event_source_fd_update(client.source, client.out.empty()
                                               ? WL_EVENT_READABLE
                                               : WL_EVENT_WRITABLE);
  return 0;
}
// Accept only bounded same-user connections from the configured Manager executable
int acceptReady(int fd, uint32_t, void *) {
  // Reap completed readiness connections before applying the connection bound
  // This callback cannot be a Client callback, so no callback deletes itself
  std::erase_if(clients, [](const auto &client) {
    char byte;
    return client->dead ||
           recv(client->fd, &byte, 1, MSG_PEEK | MSG_DONTWAIT) == 0;
  });
  int peer = accept4(fd, nullptr, nullptr, SOCK_NONBLOCK | SOCK_CLOEXEC);
  if (peer < 0)
    return 0;
  ucred cred{};
  socklen_t size = sizeof(cred);
  if (clients.size() >= MAX_CLIENTS ||
      getsockopt(peer, SOL_SOCKET, SO_PEERCRED, &cred, &size) ||
      cred.uid != getuid() || !ordinaryProcess(cred.pid) ||
      executable(cred.pid) != SDM_MANAGER_EXECUTABLE) {
    close(peer);
    return 0;
  }
  auto client = std::make_unique<Client>(peer, cred.pid);
  client->source =
      wl_event_loop_add_fd(g_pCompositor->m_wlEventLoop, peer,
                           WL_EVENT_READABLE, clientReady, client.get());
  if (!client->source)
    return 0;
  clients.emplace_back(std::move(client));
  return 0;
}
// Expire idle leases and revoke observations after suspend or stalled polling
int timerReady(void *) {
  // BOOTTIME includes suspend; never silently resume an old lease after sleep
  static uint64_t previousBootMs = 0;
  timespec boot{};
  const bool clockOk = clock_gettime(CLOCK_BOOTTIME, &boot) == 0;
  const uint64_t bootMs = uint64_t(boot.tv_sec) * 1000 + boot.tv_nsec / 1000000;
  const bool stalled =
      !clockOk || (previousBootMs && bootMs - previousBootMs > 2000);
  previousBootMs = bootMs;
  const auto now = Clock::now();
  for (auto &client : clients) {
    if (stalled) {
      client->revoked = true;
      client->detach();
    }
    if ((client->mode != "target" &&
         now - client->touched > std::chrono::seconds(2)) ||
        now - client->started > std::chrono::minutes(20))
      client->dead = true;
    else
      client->check();
  }
  std::erase_if(clients, [](const auto &client) { return client->dead; });
  wl_event_source_timer_update(timerSource, 100);
  return 0;
}
// Release clients, event sources and the compositor socket
void teardown() {
  clients.clear();
  if (timerSource)
    wl_event_source_remove(timerSource);
  if (listenerSource)
    wl_event_source_remove(listenerSource);
  timerSource = listenerSource = nullptr;
  if (listenerFd >= 0) {
    close(listenerFd);
    listenerFd = -1;
    unlink(socketPath.c_str());
  }
}
} // namespace
// Expose the compositor plugin API version required by Hyprland
APICALL EXPORT std::string PLUGIN_API_VERSION() { return HYPRLAND_API_VERSION; }
// Validate the pinned compositor ABI and register the restricted local authority socket
APICALL EXPORT PLUGIN_DESCRIPTION_INFO PLUGIN_INIT(HANDLE handle) {
  const auto runtimeVersion = HyprlandAPI::getHyprlandVersion(handle);
  if (runtimeVersion.dirty || runtimeVersion.hash != SOURCE)
    throw std::runtime_error(
        "SDM requires the clean pinned Hyprland runtime source");
  if (std::string(GIT_COMMIT_HASH) != SOURCE ||
      std::string(__hyprland_api_get_hash()) !=
          __hyprland_api_get_client_hash())
    throw std::runtime_error(
        "SDM requires the exact supported Hyprland source and dependency ABI");
  socketPath = g_pCompositor->m_instancePath + "/sdm-keyboard-v1.sock";
  sockaddr_un addr{};
  addr.sun_family = AF_UNIX;
  if (socketPath.size() >= sizeof(addr.sun_path))
    throw std::runtime_error("compositor path too long");
  std::copy(socketPath.begin(), socketPath.end(), addr.sun_path);
  listenerFd = socket(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK | SOCK_CLOEXEC, 0);
  if (listenerFd < 0)
    throw std::runtime_error("socket unavailable");
  // Restrict the socket filesystem permissions without changing the caller umask permanently
  const auto oldMask = umask(0077);
  const auto bound =
      bind(listenerFd, reinterpret_cast<sockaddr *>(&addr), sizeof(addr));
  umask(oldMask);
  if (bound) {
    close(listenerFd);
    listenerFd = -1;
    throw std::runtime_error("socket already exists or cannot be bound");
  }
  if (listen(listenerFd, MAX_CLIENTS)) {
    teardown();
    throw std::runtime_error("listen failed");
  }
  listenerSource =
      wl_event_loop_add_fd(g_pCompositor->m_wlEventLoop, listenerFd,
                           WL_EVENT_READABLE, acceptReady, nullptr);
  timerSource = wl_event_loop_add_timer(g_pCompositor->m_wlEventLoop,
                                        timerReady, nullptr);
  if (!listenerSource || !timerSource) {
    teardown();
    throw std::runtime_error("event loop registration failed");
  }
  wl_event_source_timer_update(timerSource, 100);
  return {"SDM keyboard guard",
          "Scoped physical keyboard and target authority; no injection",
          "StreamDock Manager", "0.1.0"};
}
// Release all authority resources when the compositor unloads the plugin
APICALL EXPORT void PLUGIN_EXIT() { teardown(); }
