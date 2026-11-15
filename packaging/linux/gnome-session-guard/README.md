# GNOME Session Guard

This optional GNOME Shell extension supplies read-only session state to Stream Dock Manager keyboard macros on **GNOME X11, Shell 45–48**. It does not observe keys, inject input, change permissions, lock the screen, or inhibit locking. Installing it does not enable Wayland macro playback or recording.

| Requirement     | Supported scope                                                       |
| --------------- | --------------------------------------------------------------------- |
| Desktop session | GNOME **X11**, Shell **45–48**                                        |
| Installed tool  | `gnome-extensions`                                                    |
| Installation    | Explicit user choice; Manager never installs or enables it            |
| Wayland macros  | Not enabled by this extension                                         |
| Qualification   | Source reviewed; native GNOME session/device testing remains deferred |

## Install and enable

From the **Manager repository root**, pack and install the extension:

```sh
gnome-extensions pack --force --out-dir /tmp packaging/linux/gnome-session-guard
gnome-extensions install --force /tmp/keyboard-session@streamdockmanager.shell-extension.zip
```

Log out and back in if GNOME has not discovered it, then explicitly enable it:

```sh
gnome-extensions enable keyboard-session@streamdockmanager
```

## Disable or remove

To stop using the companion:

```sh
gnome-extensions disable keyboard-session@streamdockmanager
gnome-extensions uninstall keyboard-session@streamdockmanager
```

## How session protection works

The normal GNOME ScreenSaver `ActiveChanged` signal intentionally waits until the lock-screen animation finishes. This companion instead observes Shell's session mode and screen-shield state on Shell's main loop. Manager combines that state with logind, pinned service/session/display identity and fresh per-transaction leases. Unknown state, companion removal/restart, suspension, lock, or provider changes revoke active leases. It does not promise atomic input/lock behavior across processes.

GNOME X11 admission can use an initial verified companion snapshot; it does not require KDE's bootstrap lock/unlock cycle. The companion must be owned by the same D-Bus connection as the running GNOME Shell. It uses protocol version 1, a fresh instance UUID per enable, and monotonically increasing state generations. It has no method to alter the desktop.

## Implementation and verification limits

The source interfaces were reviewed against GNOME Shell branches `gnome-45`, `gnome-46`, `gnome-47`, and `gnome-48`, specifically `js/ui/screenShield.js`, `js/ui/sessionMode.js` and `js/ui/shellDBus.js`. No `Eval`, private-method replacement or general input filter is used. `session-modes` includes the lock screen so the guard remains available to report protected state; disabling the extension still revokes its service.

This extension has **not been installed or exercised in a native GNOME session**. JavaScript syntax and Rust compilation are not evidence of actual lock/input behavior. Before release qualification, test on the exact Shell version:

- Enable/disable, restart and service replacement.
- Launch while locked and unlocked, including the lock animation.
- Suspend/resume and user switching.
- Permission behavior and real device/input behavior.

[Back to packaging](../../README.md) · [Manager keyboard recordings](../../../README.md#keyboard-recordings)
