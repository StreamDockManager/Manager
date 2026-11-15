# Stream Dock Manager

Make your Stream Dock a control panel for everyday tasks. Build pages and profiles, assign keys and dials, install plugins, and personalize each display. You can explore the editor with a simulator before connecting a device.

**Unreleased · 0.1.0 · GPL-3.0-or-later**

> This is a development project, not a published stable release. Hardware support is experimental. Check your exact model and variant, and back up your configuration before trying another build.

| I want to…                       | Start here                                                                             |
| -------------------------------- | -------------------------------------------------------------------------------------- |
| Explore without installing       | [Website demo](https://streamdockmanager.app/preview)                                  |
| Try the current app              | [Run from source](#run-from-source)                                                    |
| Install a package I already have | [Install and open](#install-and-open)                                                  |
| Create my first control          | [First profile](#set-up-your-first-profile)                                            |
| Add plugin actions               | [Plugins and Node.js](#add-plugins-and-nodejs)                                         |
| Check my device                  | [Hardware-tested devices](#hardware-tested-devices) · [All models](hardware/README.md) |
| Help develop Manager             | [Contribution guide](CONTRIBUTING.md)                                                  |

## Independence and compatibility

This is an independent project and is not affiliated with, endorsed by, or sponsored by
VSDInside, MiraBox, OpenDeck, or Elgato Stream Deck. Product names are used to identify
products and describe compatibility; they do not imply an official relationship.

## Install and open

No published installer is assumed by this guide. [Build from source](#run-from-source), or, if you already have a trusted development package, follow its platform guide:

| Platform                                    | Desktop package           | Installation guide                                                        |
| ------------------------------------------- | ------------------------- | ------------------------------------------------------------------------- |
| macOS 14+, Apple Silicon or Intel           | Architecture-specific DMG | [macOS](packaging/install-instructions/macos/INSTALL-INSTRUCTIONS.md)     |
| Windows 11 x64                              | EXE installer             | [Windows](packaging/install-instructions/windows/INSTALL-INSTRUCTIONS.md) |
| Linux x64, Ubuntu 24.04 / Nobara 44 targets | DEB / RPM                 | [Linux](packaging/install-instructions/linux/INSTALL-INSTRUCTIONS.md)     |

Packages have no trusted publisher signature. macOS desktop bundles have an ad hoc integrity seal but are not notarized. Your operating system may warn or block installation. Check source/build information and follow the platform guide; checksums detect changed bytes, not publisher identity. Do not disable system protections to install Manager.

The CLI is a separate product and is not needed to run the desktop app. See [Packaging](packaging/README.md) to create local installers.

## Requirements

These are **source-development** requirements. Installed desktop packages do not require the frontend toolchain.

| Tool                                                      | Version / purpose                                                                              |
| --------------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| [Node.js](https://nodejs.org/) and npm                    | Node **26.x**, npm **12.2.2**, as declared in `package.json`; frontend and desktop development |
| [Rust and Cargo](https://www.rust-lang.org/tools/install) | Rust **1.95 or newer**; add rustfmt and Clippy for contribution checks                         |
| Git                                                       | Clone the repository and record build provenance                                               |
| GNU Make and Python                                       | Python **3.11+** for this repository's Make wrappers                                           |

| Platform | Native prerequisites                                                                                                                                                                 |
| -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| macOS    | macOS 14+ and Xcode Command Line Tools                                                                                                                                               |
| Windows  | MSVC C++ Build Tools and Windows SDK; use MSYS2 for GNU Make or the direct npm/Cargo commands below                                                                                  |
| Linux    | WebKitGTK 4.1, GTK3, libudev and the other [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/); see the [Ubuntu setup commands](CONTRIBUTING.md#linux-system-libraries) |

The Node.js version used to **build Manager** is separate from the supported **Node.js LTS** runtime selected for JavaScript plugins. Manager installers do not bundle a plugin runtime. The CLI requires Rust and platform-native libraries, but no Node.js or frontend build.

## Run from source

### 1. Get the source

```sh
git clone https://github.com/StreamDockManager/Manager.git StreamDockManager
cd StreamDockManager
```

### 2. Install dependencies and start the desktop app

After installing the requirements above:

```sh
make deps
make desktop
```

`make deps` installs the locked npm dependencies. `make desktop` builds the native app and starts its frontend with hot reload. The first Rust build can take a while. `make dev` is an alias.

Without GNU Make, use the equivalent commands:

```sh
npm ci
npm run desktop
```

The Make wrapper also supplies the public build settings in [`workspace.toml`](workspace.toml). See [build configuration](CONTRIBUTING.md#build-configuration) if you need to override them.

### Browser-only preview

The [website demo](https://streamdockmanager.app/preview) lets you explore the simulated editor without installing the toolchain. It cannot connect devices, run plugins or native actions, or save changes across reloads. Website availability depends on deployment.

To run the development preview locally, after `npm ci` (or `make deps`):

```sh
npm run dev
```

Open the local address printed by Vite. This preview uses temporary simulated data and cannot access USB devices or execute host actions. It is useful for exploring or developing the interface, not for testing native device behavior.

### CLI only

```sh
python3 scripts/prepare_patched_deps.py
cargo build --locked --release -p stream-dock-manager-cli
cargo run --locked --release -p stream-dock-manager-cli -- --help
```

The executable is `target/release/stream-dock-manager-cli` (`.exe` on Windows), unless you override Cargo's target directory. Quit the desktop app before CLI commands that open the same configuration, or pass `--config /absolute/separate/directory` before the subcommand. The platform installation guides show how to run packaged CLI builds.

## Set up your first profile

1. Quit other device managers, including VSD Craft, before connecting your Stream Dock.
2. Open Manager and complete setup. Choose your device, or use a simulator to explore the editor.
3. Create or select a profile and page.
4. Select a control, choose an action, and adjust its label or appearance.
5. Choose **Save binding**. Install and enable a plugin before assigning one of its actions.

| Platform      | First-launch check                                                                                                                                                                                                         |
| ------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| macOS         | In **Set up permissions**, use **Open Settings** for Input Monitoring and Accessibility. Enable the installed Applications copy, return to **Check again**, then choose **Connect devices**. Screen Recording is optional. |
| Linux         | Reconnect the device after installing its USB access rules. Run Manager as your normal desktop user.                                                                                                                       |
| All platforms | Close competing device managers. Use a simulator if your exact hardware variant is not available.                                                                                                                          |

You can reopen the macOS review with **Settings → Check permissions**. If the device reports `0xE00002E2` / “not permitted”, follow the [HID access steps](packaging/install-instructions/macos/INSTALL-INSTRUCTIONS.md#hid-device-reports-not-permitted), then fully quit and reopen Manager.

**Closing the editor leaves Manager running in the tray/menu bar. Choose Quit there to stop it.**

### Preview, apply and save

Selecting an action or changing its appearance previews the display without running or saving the action. Previews appear in the editor and on a connected, awake dock. Plugins without live feedback use their declared artwork.

| Control                                     | Result                                                                                     |
| ------------------------------------------- | ------------------------------------------------------------------------------------------ |
| **Apply appearance**                        | Update the current binding draft                                                           |
| **Save binding**                            | Persist the action and appearance                                                          |
| **Cancel** in the appearance editor         | Discard that appearance draft                                                              |
| Leave the selected control                  | Restore its saved device display                                                           |
| **Reset** in **Icon, label & live display** | Restore the action's default label, icon, colors, media and layout in the appearance draft |

Reset leaves action settings and dial/activation assignments unchanged. Cancel discards the reset; Apply appearance and then Save binding keep it. New Keyboard macro actions place the icon at the top, label in the center and status at the bottom.

## Hardware-tested devices

These devices have been tested with physical hardware:

| Model           | Device notes                        |
| --------------- | ----------------------------------- |
| N4 Pro          | [N4 Pro](hardware/n4-pro/README.md) |
| N1, VSD variant | [N1 VSD](hardware/n1-vsd/README.md) |
| N3, VSD variant | [N3 VSD](hardware/n3-vsd/README.md) |
| K1 Pro          | [K1 Pro](hardware/k1-pro/README.md) |

This is not a claim that every feature, firmware version or operating system has been tested. Similarly named non-VSD variants do not inherit these results. Other registered models remain experimental and are not currently hardware-tested. See the [hardware guide](hardware/README.md) for exact variants and limits.

## Add plugins and Node.js

1. Open **Plugins** and choose a catalog download or **Import plugin** for a local ZIP.
2. Review the package, install it, then explicitly enable it.
3. Open **Plugin settings** if it needs a service connection.
4. Assign its action to a control and save the binding.

Plugins default to **Sleep** when none of their actions are visible on a device or simulator. They wake when shown or needed for settings or testing; background activity stops while sleeping. Turn off **Sleep** in a plugin's options if it needs to keep running in the background.

Existing installations retain their previous effective Sleep setting when upgrading, including off. To reduce background activity for an existing plugin, enable **Sleep** in its options. New plugins default to on, and explicit on/off choices are saved.

**Health and compatibility** shows current CPU/resident memory, current-run peaks, and five minutes of bounded trends. Missing measurements leave gaps rather than appearing as zero; browser plugin memory remains unavailable when WebKit cannot attribute it safely. The overview explains Sleep and records the last start attempt’s time and categorical reason for this Manager session, without recording action payloads.

Browse the [plugin catalog](https://streamdockmanager.app/plugins) for compatible packages. Availability depends on what has been published. Choose an OS-specific package when available; a unified package contains multiple native runtimes, not extra platform compatibility.

| Plugin type            | What you need                                                                              |
| ---------------------- | ------------------------------------------------------------------------------------------ |
| Native Go, Rust or C++ | The package for your OS and CPU                                                            |
| Packaged Python        | The interpreter included in the plugin package                                             |
| JavaScript             | A supported **Node.js LTS** runtime selected in **Settings → Runtimes**                    |
| VSD Craft / OpenDeck   | A readable standalone package; review compatibility before enabling                        |
| Stream Deck            | A supported older or unencrypted package; protected Marketplace packages are not supported |

Manager does not bundle Node.js or decrypt protected plugins. Importing copies a package without changing its source, and does not migrate another application's profiles or settings.

<details>
<summary>Import formats, compatibility, runtime management and unified packages</summary>

### Import installed plugins

**Plugins → Import installed plugins** discovers VSD Craft and OpenDeck plugin folders. Select the source application before scanning. Choose a custom folder for other standalone packages, select packages, and review their permissions before importing. Manager copies packages without changing their source. New imports stay disabled; reviewed replacements preserve the existing enabled state. Automatic import from the installed Elgato Stream Deck application is not offered. Compatibility with supported older or unencrypted Stream Deck plugins remains available through manual folder or package import, including `.streamDeckPlugin` archives. Protected Marketplace packages remain unsupported: Manager does not decrypt manifests or bypass licensing/integrity protection.

OpenDeck discovery uses `~/Library/Application Support/opendeck/plugins` on macOS, `%APPDATA%/opendeck/plugins` on Windows, and `$XDG_CONFIG_HOME/opendeck/plugins` (default `~/.config/opendeck/plugins`) on Linux. Linux also checks `~/.var/app/me.amankhanna.opendeck/config/opendeck/plugins` for Flatpak installations. Only readable standalone packages supported by Manager can be copied; this does not migrate OpenDeck profiles, settings, dependencies, or device-provider integrations. Symlinked package folders are not followed.

### Unified plugin packages

Catalog installs and downloads default to the smaller package for this operating system and CPU. When a publisher provides a unified archive, choose **Package → Unified package** explicitly before downloading or installing. Automatic updates continue to select the platform-specific package. Unified archives retain the same hash, file-inventory and permission review; they do not imply VSD Craft qualification on every included platform.

Native SDK/VSD manifests may provide `CodePaths` keyed by `x86_64` or `aarch64` target triples for Windows (`pc-windows-msvc`), macOS (`apple-darwin`) and Linux (`unknown-linux-gnu`). When this map is present, Manager requires an exact target match and checks every referenced file. It never falls back to another CPU's executable. OpenAction retains its separately documented override/fallback rules.

### OpenAction plugins

Manager implements the plugin protocol documented by the [OpenAction API](https://github.com/OpenActionAPI/docs/tree/ffe2564bd42f54b19443cf1dac821adf6a85a848), alongside its VSD Craft and Stream Deck compatibility. Qualification limits are listed below. Import an unprotected `.sdPlugin` directory through **Import installed plugins → OpenDeck** (or **Choose plugin folder** for a custom installation), or a ZIP package through package import. Review permissions and enable the copied plugin; imports never modify the source installation.

- ZIP imports accept a `.sdPlugin` bundle, a single enclosing folder, or files directly at the archive root. Explicit manifest UUIDs take priority; named `.sdPlugin` bundles must match them. If neither exists, Manager can use a shared, complete dot-separated namespace from at least two distinct action UUIDs (at least three namespace segments). Single-action or ambiguous packages need an explicit manifest UUID or correctly named bundle folder. Normalization changes only the disposable staging layout, never the source archive or manifest bytes. Multiple bundles, conflicting identities, unsafe paths and extra payload outside an enclosing folder are rejected.
- Manifests without `SDKVersion` use the shared OpenAction/legacy compatibility validator, not the current Elgato schema. This overlap does not identify the package’s original authoring SDK. Both numeric and string font sizes are accepted. Compatible OpenDeck and older SDKVersion-less Stream Deck packages may retain one-to-four-part numeric versions and legacy `Software` metadata; that metadata is not an OpenAction host-version requirement. Platform/CPU-specific `CodePaths` take precedence over `CodePathMac`, `CodePathWin`, `CodePathLin`, then `CodePath`. Native executables, Node.js LTS, and HTML plugins retain the same package confinement and permission review.
- Supported protocol paths include registration, action lifecycle, keys/dials/touch, state images/titles, encoder feedback, opaque JSON action/global settings, and isolated property-inspector messaging. Two-state actions toggle on release. OpenAction's `actionDefaultImage`, SVG-first icon lookup, and nullable visual resets are supported.
- Plugins declaring `HasSettingsInterface` show **Open plugin settings** while connected. This requests the plugin's own window; it does not grant additional Manager capabilities.
- `openaction://plugins/message/<plugin-directory>/<message>` routes to an enabled, running plugin by its installed directory. `?openaction=hidden` suppresses Manager's own foreground request. **Settings → Handle OpenAction plugin links** is independent of the Stream Deck link preference and off by default. OS handler changes require explicit confirmation; packaged macOS can restore an existing previous handler. Automatic handler registration on Linux/Windows and registration without a restorable prior macOS handler are not implemented.
- Application launch/termination and system wake events use native macOS/Windows sources. Linux uses executable-name snapshots and logind's `PrepareForSleep` resume signal; systems without logind report unavailable wake observation. Windows/Linux process polling may miss processes that start and exit between one-second samples. Only declared executable identities are retained, not command lines or window titles.
- Compatibility tests use the genuine [OpenAction Rust SDK 2.8.0](https://github.com/OpenActionAPI/rust/tree/d435b2d57d6df13debaca330011a3067f66a99db) and the original System Information/Redline sample manifests. Redline is Linux-only; accepting its manifest does not make its Linux executable run on macOS or Windows. Native Linux/Windows execution, real URL activation, browser presentation, and physical-device behavior still need platform qualification.
- OpenDeck-specific platform manifest overrides, profiles/settings migration, Marketplace downloads, and device-provider integrations are separate from this documented plugin API. Packages containing a `manifest.<current-os>.json` override remain blocked during review instead of silently ignoring it. Manager does not decrypt protected Elgato packages.

### Manage Node.js LTS

JavaScript plugins use **Node.js LTS**. Open **Settings → Runtimes** to install a private runtime or choose an existing installation. You can select any offered release from a supported LTS line; the exact selection is retained when new releases appear. Runtime metadata checks do not automatically install or upgrade Node.js.

Expand **Node.js installation details** to delete an unused private runtime with its trash icon. Manager asks for confirmation and never deletes existing system or user-managed installations. Stop plugins using the runtime first, or select a replacement and restart them. Saved selections are retained after deletion and report the runtime missing until it is reinstalled or replaced.

Native Go, Rust, and C++ packages run their compiled executables. Packaged Python plugins include their own interpreter. Manager installers do not bundle Node.js.

</details>

## Automatic profiles and pages

Open **Device editor → Automatic profiles** to switch profiles for an application or a local-time schedule.

1. Choose **Browse application** or **Use last focused application**. On macOS, an `.app` selection resolves its bundle ID.
2. Select the profile and page. **Remembered page** keeps that profile's previous page.
3. Optionally add a window-title condition or schedule, save the rule, and enable automatic switching.
4. Choose **Apply** to save the dialog. **Cancel**, Escape or closing it discards both rule and switching-state changes.

| Setting or action        | Behavior                                                                                         |
| ------------------------ | ------------------------------------------------------------------------------------------------ |
| Rule order               | Rules run top to bottom when the focused app or winning rule changes                             |
| Manual page change       | Remains selected after the initial automatic switch                                              |
| Return to Manager        | Does not count as switching to another application                                               |
| Manual profile selection | Pauses automatic switching until you resume it                                                   |
| Window-title condition   | **Contains**, **Exact**, or Rust **Regular expression** syntax; no look-around or backreferences |
| macOS title matching     | Requires Accessibility access                                                                    |
| Wayland                  | Schedule-only rules                                                                              |

<details>
<summary>Returning to the previous profile or page</summary>

**Return to previous profile/page when no rule matches** is off by default. Enable it to return to your original location after leaving matching applications. Another matching rule takes priority. Page changes within the automatically selected profile preserve the original return location; manually selecting another profile clears it. The return location lasts only for the current Manager session.

If another window changes the configuration while this dialog is open, cancel and reopen before trying again.

</details>

## Configure dial actions

Select a dial and assign each input independently:

| Input          | Example                                    | New-action defaults |
| -------------- | ------------------------------------------ | ------------------- |
| **Turn left**  | Decrease volume or go to the previous page | Decrease / Previous |
| **Turn right** | Increase volume or go to the next page     | Increase / Next     |
| **Press**      | Mute or open a specific page               | Specific target     |

Turn on **Link turn actions** to edit one action for both rotation directions. Relative
volume and page/profile navigation reverse for the other direction; plugins still receive
signed dial input. Press stays independent. Linking asks before replacing a different right
assignment. Turn linking off to edit each direction separately.

Choose **Unset** on any input to clear it. Clearing one linked turn separates the directions
and leaves the other turn intact. Changes remain drafts until you choose **Save binding**.

Each input can run a multi-action sequence. You can override either rotation direction, including individual multi-action steps; existing saved choices are preserved. Page/profile navigation wraps at the ends and skips profiles for other device models. Choose **Save binding** to keep all three assignments.

### Copy an assignment to other pages

To reuse a key, dial, touch region, or display assignment in the same position, choose **Save assignment to** in the inspector: **This page**, **Every page in this profile**, or **Every page in all matching profiles**. Matching profiles must use the same device model. Review the page/profile and replacement counts, confirm replacement, then save. This copies the complete binding, including appearance and all dial inputs, in one undoable transaction; unrelated controls stay unchanged. These are independent copies, not linked assignments or defaults for future pages. Absolute page targets must exist in every destination profile; use Previous/Next navigation for portable page switching.

Dial bindings use explicit independent left, right, and press inputs. Obsolete assignment formats are rejected rather than silently converted; original saved data is preserved. Press is available only on devices that report a dial press.

## Keyboard recordings

Choose **Keyboard macro → Edit recording**. The action is called **Keyboard macro** in the editor; its default hardware label is **Macro**.

| Step                                               | What happens                                    |
| -------------------------------------------------- | ----------------------------------------------- |
| Record in **In Manager**                           | Capture keys only in the focused recording area |
| **Stop**                                           | End recording and review the timeline           |
| **Simulate**                                       | Preview timing and counts without sending keys  |
| **Apply**, then **Save binding** or **Save macro** | Keep the recording                              |
| **Cancel**                                         | Leave the original draft unchanged              |

Recordings store physical key positions, not text, so changing keyboard layout can change their output. Never record passwords. Saved recordings are included in workspace exports and backups.

> **macOS:** lock detection is best effort and was observed about ten seconds after locking. Stop recording or playback before locking or switching users. Native behavior still needs real-device verification.

<details>
<summary>Recording modes, platform limits and safe playback</summary>

### Capture and review

Choose **Keyboard macro** in the keyboard/text actions, then **Edit recording**. Its default hardware label is **Macro**; custom labels are preserved. Record only in the focused capture area, or add physical keys manually. **Stop** ends capture and adds releases for any keys still held; the review reports those additions. Escape is recordable while the area is focused. Use **Stop**, **Cancel**, or **Ctrl+Shift+F12** to end capture. Focus leaving the capture area stops in-app recording. Initially held keys must be released before capture starts.

**In Manager** is the default recording mode. **Another app** is shown separately and requires an available native backend and existing permissions. That mode uses a three-second countdown, a persistent status/Stop/Cancel area and its own displayed emergency shortcut (Ctrl+Shift+F11 on Windows). Changing focus does not end an explicitly selected global recording.

Stop and Cancel wait for recording cleanup without freezing the editor. Cancel remains available while Stop is pending. If native cleanup takes too long, the draft stays locked and you can retry cancellation; Manager does not claim the recorder is released or allow competing keyboard output before teardown finishes. Quit also waits for cleanup and keeps Manager open if it cannot confirm completion.

The editor preserves press/release order, overlaps, explicit repeat events and millisecond delays. Review the timeline before applying. **Apply** changes the parent action draft; **Save binding** or **Save macro** persists it. Canceling either editor leaves that editor's original draft unchanged. Recordings can be used in bindings, dial inputs, multi-actions and saved macros. **Simulate** previews timing and counts without sending keys. Selecting an action, importing a profile or loading saved data never starts capture or playback.

Recordings use physical keyboard positions, including left/right modifiers, rather than text. Changing keyboard layout can change the produced characters. Unsupported physical keys, composition/IME input and unbalanced edits must be corrected before Apply. Recordings are limited to five minutes, 10,000 events and 1 MiB; playback speed is 0.25×–4×. Existing repeat/multi-action budgets also apply.

### macOS and Windows

macOS global capture, playback and scratch tests use live Input Monitoring, Accessibility/event-posting and session checks. macOS lock detection is **best effort**: a local test observed about a ten-second delay after the screen locked. Stop recording or playback before locking or switching users, and do not trigger macros while locked. This accepted tradeoff is also shown in the editor; healthy observations and secure-input checks are not a guarantee that a target is safe. Unknown/protected states, permission loss, observed target changes, cancellation and observer failures still stop the session. Actual native key receipt, repeat fidelity and the full OS/device matrix remain unverified; simulation/browser checks are not native qualification.

Windows uses live unlocked-session/default-desktop checks, scoped Raw Input/hook corroboration, a reserved emergency shortcut, target/integrity checks and checked SendInput output. Its real-desktop delivery matrix remains unverified.

### Linux X11

Linux X11 supports local KDE Plasma with its early-lock interface, or GNOME Shell 45–48 with the optional [GNOME Session Guard](packaging/linux/gnome-session-guard/README.md) explicitly installed and enabled. Both bind the concrete logind session and display. KDE requires an observed lock/unlock cycle after ordinary startup; the GNOME companion can provide an initial verified state. Manager never locks the desktop. Global capture combines XI2 physical provenance with RECORD server repeat events. Playback and scratch tests use session leases and pinned window/client/process lifetimes.

New Keyboard macro actions on Linux X11 default to **System repeat (Linux)**, the recommended option for ordinary shortcuts and held keys. Existing saved choices are preserved; older actions without a repeat-mode field retain Exact behavior. **Exact recorded repeats** remains an option and the default on other platforms. On X11, Exact accepts only keys that cannot auto-repeat under the existing source/master controls, with ordinary momentary modifiers. With System repeat: recorded Repeat entries are not sent, and the system supplies repeat delay/rate. Repeat count/timing may differ, including for recordings without Repeat entries. Playback speed changes press/release timing, not system repeat settings; simulation reports recorded checkpoints, not a predicted output count. On X11, latch/lock/group effects and unsupported mappings still fail before output. macOS and Windows do not accept System mode.

### Hyprland and other Wayland sessions

Hyprland Wayland has an optional [Keyboard Guard companion](packaging/linux/hyprland-keyboard-guard/README.md), pinned to Hyprland 0.56.2 and its exact source/dependency ABI. It must be built and loaded explicitly for the installed Manager binary; Manager never installs or loads it. It supplies scoped physical-input observation and native Wayland target identity. Playback separately requires a compatible RemoteDesktop portal provider and its keyboard authorization. The [stock Hyprland portal](https://github.com/hyprwm/xdg-desktop-portal-hyprland/blob/master/hyprland.portal) currently does not implement RemoteDesktop, so the companion alone does not enable playback on a stock installation. Select **System repeat** explicitly before global recording or playback: this backend records physical presses/releases and holds, not client-generated repeat counts. Lock/focus events and bounded session checks revoke observed unsafe state; they are not an atomic guarantee against every intervening desktop transition. Physical keys follow the configured compositor keymap: custom remaps can cause latch/lock/group effects before the guard observes the change. Playback then stops; owned-key cleanup does not restore those effects. The companion has compiled against the pinned Arch Linux x86-64 SDK; it is not qualified on a real compositor or as an Ubuntu release. See the companion README for build artifacts and exact ABI requirements.

Other desktop session providers and generic Wayland native macros remain unsupported. Extra Wayland portal permission alone cannot supply physical observation or focused-target identity. No shared repeat settings or input permissions are changed, and no XWayland fallback is used. Native/device testing is deferred.

### Playback and scratch-target safety

When native playback is available, the default policy stops on a target application/window change. **Allow focus changes** is an explicit alternative: subsequent keys may reach a different application. Physical interference, lost permissions and cancellation stop the run, with best-effort release of only that run's keys. Overlapping keyboard runs are rejected rather than queued, and other keyboard/plugin input cannot interleave with a recorded sequence. Abrupt process termination or power loss cannot guarantee releases; recordings are never automatically resumed after restart.

**Test playback** uses a separate scratch-target workflow and requires the platform's live prerequisites. Choose a disposable scratch window during the first three-second countdown, review its application/window label, then explicitly confirm the test. A second countdown lets you return to that same window. Tests always stop on a target change, regardless of the saved action's focus policy. The draft stays locked while testing or canceling; cancellation waits for native teardown before reporting completion. Browser preview never selects a native target or sends keys.

X11 scratch-target observation pins server-owned window/client identity, subscribes to destruction before accepting the target, and checks actual keyboard-focus ancestry. Process metadata is conservative liveness information, not proof of an original connection-owner identity. This path still needs desktop-specific session and native target-lifetime qualification.

### Privacy

Recordings can contain sensitive keystrokes. Use a blank scratch application and never record passwords. Password fields cannot be detected reliably across applications. Saved recordings are included in ordinary workspace exports and backups, which are **not a password vault**. Raw session events are not sent to plugins or diagnostic history. Recorder IPC is restricted to the owning main editor document and expires if its heartbeat is lost. No recording hook runs at startup or while idle.

</details>

## Useful commands

Run commands from this repository root.

| Command                | What it does                                                                     |
| ---------------------- | -------------------------------------------------------------------------------- |
| `make build`           | Build the native debug app and production frontend                               |
| `make package`         | Build local release-mode desktop installers for the current platform             |
| `make build-cli`       | Build the separate release-mode CLI executable                                   |
| `npm run build`        | Type-check and build the browser preview                                         |
| `npm run preview`      | Serve the built browser preview                                                  |
| `make test`            | Run workspace Rust tests                                                         |
| `make check`           | Check generated types, frontend build, hardware descriptors and Rust compilation |
| `npm run format:check` | Check Prettier and rustfmt formatting                                            |
| `make help`            | List build commands                                                              |

For macOS installers, use `make build-macos ARCH=arm64` (Apple Silicon) or `make build-macos ARCH=amd64` (Intel). Omit `ARCH` or use `ARCH=both` for two separate installer sets. Add `PRODUCT=desktop` or `PRODUCT=cli` to build only one product.

See [Packaging](packaging/README.md) for target builds and [Contributing](CONTRIBUTING.md) for validation and review. A release-mode build is not a published or qualified release.

## Troubleshooting

| Problem                                     | Try this                                                                                               |
| ------------------------------------------- | ------------------------------------------------------------------------------------------------------ |
| A device is missing                         | Check its exact [model/USB variant](hardware/README.md), close other device managers, and reconnect it |
| A plugin action does nothing                | Check that the plugin is enabled, its settings are applied, and the binding is saved                   |
| A JavaScript plugin cannot start            | Select a supported Node.js LTS installation in **Settings → Runtimes**                                 |
| Plugin startup prevents normal use          | Choose **Restart in recovery mode** from the tray; plugins stay paused                                 |
| You need plugin diagnostics                 | Open **Logs ▾** on the plugin card, start a capture, then stop and download it when finished           |
| The app still runs after closing its window | Choose **Quit** from the tray/menu-bar menu                                                            |

### Configuration recovery

Automatic recovery retention keeps up to **32 files or 1 GiB**, whichever limit is reached first,
across **all files recursively inside `recovery/`**. Older generations, manual snapshots, unknown
file types, and nested quarantine evidence share this budget. Oldest files (by modification time)
are removed first after a validated automatic snapshot is published. The just-published snapshot is
always preserved, even if it alone exceeds 1 GiB. Cleanup is deferred during unfinished restoration;
active recovery operations hold the same exclusive owner lock. These safety exceptions can temporarily
exceed the budget. Symbolic links are counted and unlinked, never followed. Exported backups outside
`recovery/` are unaffected; recovery browsing has separate bounded inspection limits.

**Recovery mode** opens Configuration recovery first, so you can inspect and manually edit saved
items even when they are valid. Manager also opens it when saved configuration prevents
startup, including an unreadable primary database. Devices and plugins stay stopped while this page is open. Your original data stays intact
until you choose an action.

1. Choose **Edit JSON** for a saved item and review any reported issues.
2. Use **Find in JSON** to search, or select a located issue to jump to its highlighted field.
   Correct the JSON, then choose **Validate**. Validation does not save changes.
3. Choose **Save** to validate again and write the corrected item back to Manager's SQLite
   storage. Manager creates a backup before changing the database.
4. Once the saved data is valid, select **Continue to Manager** and choose **Continue in recovery
   mode** or **Restart normally**. Cancel keeps this page open. Saving alone never leaves recovery.

Use **Open data folder** to inspect the current configuration directory, including its database,
logs and recovery backups. Continuing in recovery
mode opens the regular Manager interface with automatic plugin startup disabled; restarting
normally resumes normal operation.

Use **⌘F / Ctrl+F** in the editor to search, **Enter / Shift+Enter** to move through matches,
and **Escape** to return to editing. Error highlights describe the last validated JSON and clear when
you edit it. Issues without a reliable location stay in the list; missing-field markers point to
the containing object.

Choose **Compare & versions** in the editor to compare your current text with saved JSON. For
profiles, you can also select a retained undo/redo version or a local recovery backup. Only actual
retained versions are listed; invalid, unrelated, or unavailable sources are skipped or explained.
Choose **Load into editor** to review a valid version, then **Validate** and **Save** to apply it.
Loading does not save anything. This restores only the selected profile’s JSON, not media files,
plugins, shared macros, or the rest of the workspace. Large comparisons and version scans are
bounded, with limits shown in the interface.

Under **Recover from a database backup**, **Copy selected profiles** adds only profiles you explicitly select to the current database.
Copies receive new identities; conflicting names get a recovered suffix. Select mutually referenced
profiles together. Missing macros, media, or managed plugin resources must be resolved before transfer;
this is not a whole-database restore and does not recover settings, plugins, shared macros, or history.
Sources remain unchanged, and the selected batch is validated and committed atomically after a
backup of the current database. You can also continue without transferring anything.

To replace the **whole configuration**, choose a backup and **Preview full restore**. Review the
validation results and warnings, then explicitly confirm replacement. This restores compatible
current-format profiles, saved macros, preferences, plugin state, and history together. Preview is
read-only; there is no automatic fallback restore. The current database, its sidecars, and interrupted
restore evidence are preserved in a uniquely named quarantine directory before replacement, and the
selected backup remains unchanged during restoration. Preserved backups and quarantine files remain
subject to the shared recovery-directory retention policy after recovery completes. Permanent media-deletion records are reconciled, so old references
to permanently deleted media are removed rather than resurrected. The recovery page stays open after
restoration until you choose Continue.

Full restore does not recreate external media, plugin binaries, or other files outside SQLite.
Missing live media or managed resources block restoration; unavailable historical/draft files and
external plugin installations are disclosed separately. If no compatible, valid backup is available,
Manager preserves the corrupt database rather than silently resetting it. A failed multi-file restore
remains fenced against ordinary startup and can be explicitly retried after reviewing a backup again.

Additional editable saved items include macros, desktop/accessibility preferences, offline-icons and
diagnostics-sampling choices, device-power settings, resource-alert settings, and library organization.
Only existing records are offered. Deleting an optional preference resets it to its normal default.
Macro identities remain fixed, and referenced macros cannot be deleted until their callers are
resolved. Credentials, plugin grants, database metadata, deletion records, and restart flags are not
exposed as editable items.

You can instead delete an affected item after confirmation. Deleting a saved editing draft loses
that unsaved editing session, not its saved profile. Deleting a profile also removes its automatic
rules, session navigation references and local parameter overrides. Invalid undo/redo entries can
be removed separately without changing the current profile. Recovery does not convert obsolete formats
or restore retired features automatically. Structural corruption cannot be edited as JSON; use a validated full backup or retain the
original database and recovery evidence for further repair.

Development data uses SQLite with one current schema and record contract. Older databases, obsolete saved formats, and incompatible same-version schemas are rejected, not migrated or reset. Rejection preserves the database and its recovery sidecars. If `manager.sqlite3` is missing, Manager creates a fresh database while preserving existing files and backups. Orphan database sidecars and interrupted-recovery markers are moved into a uniquely named preservation directory before initialization. With no recoverable SQLite backup, startup proceeds as a new installation. When recoverable backups are available, configuration recovery lets you choose a backup, review its profiles, and explicitly transfer selected valid profiles into the new database. Nothing is restored automatically or overwritten in the backup.

When the development contract changes, keep the previous configuration directory separately and select a new, empty test directory. For a separate test configuration, quit Manager and launch with an explicit path:

```sh
npm run desktop -- -- -- --config /absolute/path/to/test-config
```

Keep any test configuration you want to retain outside disposable build/artifact folders. Use **Open application data folder** in Settings to locate normal app data.

**Diagnostics → Export diagnostic report** opens an exact JSON preview before saving.
The report includes allowlisted build, OS, desktop/session/backend information, runtime
versions, plugin identifiers and performance summaries. It excludes raw logs, profile contents,
settings, hostnames, device serials and user file paths. Review the contents before sharing;
Manager does not upload it. The preview expires after ten minutes. Linux WebKitGTK and GTK
versions come from the loaded libraries; unavailable engine versions stay unknown. The
separately labeled development metadata checks are not runtime version evidence.

### Patched native dependencies

Native build wrappers automatically prepare checksum-pinned Rust dependencies and apply
the reviewed files in `patches/`. Expanded sources stay in ignored `vendor/.cache/`, not
Git. Before using Cargo directly, run `python3 scripts/prepare_patched_deps.py`; use
`--offline` with a warmed cache for offline builds. See [contributor setup](CONTRIBUTING.md#prepare-patched-rust-dependencies).

## Advanced reference

<details>
<summary>Cache storage and retention</summary>

**Settings → Download cache** shows cache size, entry counts, and limits, with a confirmed **Delete cache** action. Automatic cleanup runs once at startup and hourly while Manager is running, independently of automatic update checks.

- `download-cache`: 1 day since last use, 2 GiB, and 32 entries.
- `updates/downloads`: 2 days, 4 GiB, and 128 entries.
- `updates/cache` (Update metadata): 1 day, 1 GiB (1,073,741,824 bytes), and 64 entries.

Each cache folder enforces its own age, byte, and entry limits. Oldest idle entries are removed first. Active transfers and package consumers are protected. Manually downloaded Manager installers, unrecognized legacy files, and symbolic links are preserved; protected usage can prevent a folder from reaching its limit. Delete cache clears only recognized idle cache data, never installed plugins, installed runtimes, profiles, or saved settings. Unrecognized content is reported when its size cannot be measured safely.

</details>

<details>
<summary>Maintained compatibility boundaries and conformance</summary>

- Hardware: every registered model remains in the registry, including the older N3 variant. Unknown H1 firmware keeps its image-output restrictions until geometry is physically verified.
- Linux: Wayland portal input remains supported where the desktop provides it. X11 and other platform adapters are separate paths, not evidence of Wayland qualification.
- Media: native/FFmpeg decoding, offline icon assets, and supported audio/GPU alternatives remain available. Missing optional services produce explicit availability errors.
- Recovery: device-output recovery and compatible database backup/restore remain supported. They never authorize migrating or resetting an unsupported database.
- Plugins: current vendor manifests and protocol compatibility are tracked against pinned VSD Craft and Elgato sources in the [compatibility matrix](tests/fixtures/plugins/compatibility-matrix.json). Partial or unverified rows are not a full compatibility claim. Platform-specific executables still require their own platform.

The production Elgato host advertises a **7.1 subset**, not full Stream Deck compatibility. The pinned genuine SDK 3.0.4 default settings, resource APIs and device-change gates have executable conformance coverage. Version admission does not qualify unsupported features or untouched upstream applications. Bundled Elgato profiles support a separately confirmed, matching-geometry, single-page V2 keypad subset with confined Unix and Windows local-disk reads. The six unmodified current SDK Lights-Out archives provide source evidence; imported per-state typography is retained, generated profiles are owner-mapped, and replacement/decline/stale-review boundaries are enforced. Readonly profiles, unlinked/custom-name action edits, V3, multipage/folders and resources remain unavailable. The Windows reader rejects UNC/network/SUBST roots; its cross-build checks do not qualify native Windows behavior. This is not reference-host or hardware qualification. The macOS isolated inspector/popup path has disposable real-WebKit lifecycle and isolation tests. Real consent/user activation, native picker and permission behavior remain unqualified; Windows/Linux inspectors retain the no-popup sandbox. Hardware/reference-host qualification is incomplete. See the matrix for precise supported behavior and limits.

Static connection metadata lives in [`connection-contract.v1.json`](crates/plugin-host/connection-contract.v1.json). It is schema-validated and embedded at build time. An independent implementation registry limits advertised capabilities; run `python3 scripts/check-connection-contract.py` to execute its required conformance tests. Editing metadata alone cannot add supported behavior. Runtime identity, permissions, devices and session state remain runtime-derived, while the package version comes from build metadata.

Run `python3 scripts/vendor-compatibility.py` to verify source hashes and evidence consistency. The optional audit `python3 scripts/vendor-compatibility.py --require-full` fails while any required contract lacks verification; it is not a publication gate for the initial public release. Release preparation requires successful builds and package integrity/provenance checks, not exhaustive vendor or physical-device qualification. Refresh pinned source versions, licenses, hashes, contract fixtures and matrix evidence together when upstream changes. Mock protocol tests do not qualify real vendor hosts or hardware.

</details>

## Explore the source

| Folder                                           | Contents                                                |
| ------------------------------------------------ | ------------------------------------------------------- |
| `src/`, `public/components/`                     | Frontend controllers and templates                      |
| `src-tauri/`                                     | Desktop entry point and native commands                 |
| `crates/runtime/`, `crates/storage/`             | Device runtime and durable application data             |
| `crates/plugin-host/`, `crates/plugin-runtimes/` | Plugin execution and optional Node.js installations     |
| `hardware/`                                      | Device adapters, descriptors, and verification fixtures |
| `packaging/`, `xtask/`                           | Installers, source generation, and validation           |

Read [CONTRIBUTING.md](CONTRIBUTING.md) before changing the application. Manager is independent of VSDInside, MiraBox, Elgato, and Nobara. License: [GPL-3.0-or-later](LICENSE); dependency notices are in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

### Profile versions and plugin recovery

**Versions** beside the profile selector, or **Settings → Profiles & portability → Profile versions**,
compares the current saved profile with retained undo/redo history. Review the diff and explicitly
confirm restoration. Restore is revision-checked, validated, limited to that profile and undoable;
it does not restore plugin packages, media files or shared macro definitions. Unsaved inspector
edits are not included. Older local backups remain available through configuration recovery.

Repeated plugin failures show their recorded ten-minute frequency and last failure. Automatic
recovery has a three-restart budget per application launch session, replenished only by deliberate
Retry, Restart or re-enabling a blocked plugin, not by a short successful run. **Disable for now**
keeps the installed package and settings; it remains disabled until explicitly enabled again.

### ZIP compression

New Manager-owned ZIP outputs use DEFLATE (method 8), level 6: CLI distributions,
portable profiles/resources, reviewed plugin exports and prepared media archives.
Archive readers retain their existing compatibility with permitted legacy methods;
limits, path validation, integrity checks and permissions are unchanged. Existing
saved media and backups are not recompressed in place. Native installer formats
and externally downloaded archives retain their own format requirements.
