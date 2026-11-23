# Hyprland Keyboard Guard

An optional, source-only Hyprland plugin that lets Manager observe physical
keyboard input for recording and identify native Wayland windows. It cannot inject keys.

| Goal                 | What you need                                                                                                                 |
| -------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| Record physical keys | This companion built for your exact compositor and Manager binary; recording preserves edges and holds, not client autorepeat |
| Play a macro         | A separate RemoteDesktop portal provider, keyboard authorization, and **System** repeat mode                                  |

Stock xdg-desktop-portal-hyprland does not provide RemoteDesktop. Installing this
companion alone does not enable playback. Manager does not install a bridge or
inject keys directly.

## Check compatibility first

| Requirement   | Exact scope                                                                                                   |
| ------------- | ------------------------------------------------------------------------------------------------------------- |
| Compositor    | Hyprland **0.56.2**                                                                                           |
| Source commit | `efb50993780079460b0cbed1363e2166a2de1d9f`                                                                    |
| ABI           | Matching compositor dependency ABI, not only a matching version string                                        |
| Manager       | The exact installed binary selected when building                                                             |
| Playback      | Separate compatible RemoteDesktop portal provider, keyboard authorization and explicit **System** repeat mode |
| Status        | Source-only, optional; native compositor/input qualification remains deferred                                 |

Other versions are rejected rather than assumed compatible. This uses Hyprland's native plugin interface and must be rebuilt and reviewed when its ABI changes.

> Loading this module executes native code inside the compositor. Building it does not install or load it. Review the source and target environment before choosing to load it.

## Build on the target Linux machine

Install these prerequisites from your distribution or official upstream sources:

| Tool or library                           | Requirement                                |
| ----------------------------------------- | ------------------------------------------ |
| Hyprland development SDK and dependencies | Exact source and matching ABI listed above |
| CMake                                     | 3.25+                                      |
| C++ compiler                              | C++23 support                              |
| pkg-config                                | Available on `PATH`                        |
| Wayland                                   | Server development files                   |
| nlohmann-json                             | 3.11+                                      |

From the **Manager repository root**, substitute the absolute path to your installed Manager executable:

```sh
cmake -S packaging/linux/hyprland-keyboard-guard -B build/hyprland-keyboard-guard \
  -DSDM_MANAGER_EXECUTABLE=/absolute/path/to/streamdock-manager
cmake --build build/hyprland-keyboard-guard
```

`SDM_MANAGER_EXECUTABLE` must be the actual existing, installed Manager binary, not a launcher, renderer, shell, or development runtime. The build canonicalizes this path; the compositor checks each peer's kernel-reported PID, all four UIDs and `/proc/PID/exe`. Manager replacement at that path is trusted as part of the user's installation, not a sandbox boundary against a compromised same-user account.

Building does not install, load or enable anything. Loading a compositor plugin grants native code execution inside the compositor and requires the user's separate, explicit choice. Use Hyprland's documented manual plugin-loading workflow only after reviewing/building this module. No load or startup configuration is performed by Manager.

## Scoped protocol and safeguards

<details>
<summary>Socket protocol, leases, target identity and cleanup</summary>

The compositor creates a mode-0600 Unix socket at its own instance directory, `sdm-keyboard-v1.sock`. Existing paths are never replaced. Requests and responses are bounded newline JSON, with no request pipelining. Protocol version is 1; the handshake reports the exact source hash. Operations are `hello`, `start` with `record`, `observe` or `target` mode, `poll`, `alive`, `focus`, and `stop`. Events are `[evdevCode, pressedBoolean, elapsedMilliseconds]`. Responses include initial/current `held` keys, monotonically increasing `seq`, sticky `revoked`/`interference`, protection and target state.

Only one input lease may exist. Separate target leases survive the scratch-target confirmation and countdown. Input listeners exist only for an active recording/observation lease; target leases do not observe keys. Physical keyboards are distinguished using the compositor's device provenance; virtual keyboards, including portal output, are not treated as physical interference. Initial-held keys are exposed to Manager, which rejects nonempty admission. Initially latched modifiers are rejected by the companion; ordinary depressed modifiers and unchanged locked modifiers are not conflated with latches. Device inventory changes, keymap changes, unknown source event accounting, colliding physical ownership, queue overflow and lock/session protection revoke admission. Physical interference does not detach the observer prematurely, so Manager can still track releases for bounded output cleanup.

A target lease pins the native Wayland window and focused surface, excludes Manager itself and elevated/foreign-UID processes, and rejects XWayland. `alive` allows focus to leave during confirmation; `focus` arms sticky focus-loss protection. This plugin does not focus windows or close applications. Input leases expire after two seconds without requests; all leases expire after twenty minutes, including target confirmation leases. Disconnect, STOP and unload detach listeners. No key data is logged or persisted.

</details>

## Verification status

Compilation and linking passed in an isolated Linux x86-64 Docker builder using the signed Arch Linux Hyprland 0.56.2-4 SDK (2026-10-03 package snapshot). The SDK header identifies the exact clean upstream source above. Static ELF inspection found the three required plugin exports and resolved all strong undefined symbol names against that packaged compositor and declared library exports. This does not load the module or qualify its ABI on another installation. Compositor/input qualification remains deferred. Before relying on this provider, compile against the exact SDK and qualify initial-held keys, simultaneous keyboards, physical/virtual provenance, portal playback, target replacement, layer/IME focus, lock/unlock, suspend/session switching, plugin unload, stalls, overflow and cleanup on the actual compositor. Do not use a generic XWayland or privileged evdev fallback when this provider is unavailable.

### Timing and keymap limits

Lock requests, armed target focus loss/window close, keymap and layout changes have event subscriptions. Session-active, DPMS, seat/IME grabs and window input-block state are checked at requests and on the 100 ms timer, not by synchronous transition callbacks. A transition that begins and ends entirely between checks can therefore be missed. BOOTTIME detects suspend/long stalls, but does not establish synchronous ordering between an external portal injection and a guard check. These are explicit limitations, not qualified guarantees.

Input admission requires at least one enabled, allowed physical keyboard, while retaining all known devices for provenance accounting. Playback observation revokes continued output if any keyboard changes its latched/locked modifiers or layout group; ordinary depressed modifier changes are allowed. Physical keys use the user’s current compositor keymap. A custom remapping of an ordinary key can itself trigger a lock, latch or group action before that change is observed. This provider does not preflight arbitrary keymap actions, and releasing owned keys does not undo those custom actions or restore global modifier state.

## Build with the isolated x86-64 SDK

Use this alternative to produce a build artifact without installing Hyprland dependencies on the host. Docker must already be available with `linux/amd64` support.

The optional `Dockerfile` uses a digest-pinned official Arch Linux builder and the
2026-10-03 signed package snapshot, requiring the `hyprland 0.56.2-4` SDK and verifying
its embedded upstream commit. It downloads official package files directly and then
requires package signatures during offline installation. This avoids the emulated
pacman downloader's seccomp incompatibility without disabling either its sandbox or
Docker isolation. Dependencies are installed only inside the disposable builder.

From the Manager checkout:

```sh
docker build --platform linux/amd64 --progress plain \
  --build-arg SDM_MANAGER_EXECUTABLE=/usr/bin/stream-dock-manager \
  --output type=local,dest=artifacts/hyprland-keyboard-guard-linux-amd64 \
  packaging/linux/hyprland-keyboard-guard
```

This is an **Arch Linux x86-64 SDK artifact**, not a qualified Ubuntu release.
The output contains the module, package URLs/checksums, full SDK version inventory,
compiler manifest and ELF metadata. Matching upstream version alone is not sufficient
for loading: the target must match the source/dependency ABI and satisfy the listed
shared-library and symbol-version requirements. Nothing is installed or loaded by this
command, and no compositor or keyboard test is run.

Cross-builds explicitly set `SDM_CROSS_BUILD_TARGET_PATH=ON` and provide the canonical
absolute **target** Manager path. No fake executable is created in the builder. The
configured example follows the Linux package's `/usr/bin/stream-dock-manager` path;
verify its realpath on the actual target before loading. Native builds keep the default
existence check and resolve the local executable's realpath. Neither mode broadens the
runtime executable allowlist or grants arbitrary same-user processes input access.

[Back to packaging](../../README.md) · [Manager keyboard recordings](../../../README.md#keyboard-recordings)
