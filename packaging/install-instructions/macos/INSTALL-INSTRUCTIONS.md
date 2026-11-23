# Install Stream Dock Manager on macOS

[Download Manager](https://github.com/StreamDockManager/Manager/releases/latest) · [Build from source](https://github.com/StreamDockManager/Manager#run-from-source)

| Your Mac      | Download label | Minimum OS |
| ------------- | -------------- | ---------- |
| Apple Silicon | `macos-arm64`  | macOS 14   |
| Intel         | `macos-amd64`  | macOS 14   |

The desktop app ships as a DMG. The separate, optional CLI ships as a DMG or portable ZIP. Keep the notices and license with either download.

## 1. Check the package

Download from a source you trust. Compare SHA-256 with `SHA256SUMS` and size with `catalog.json` (desktop), `cli-catalog.json` (CLI), or the supplied development catalog:

```sh
file="$HOME/Downloads/your-manager-package.dmg"
shasum -a 256 "$file"
wc -c < "$file"
```

Stop on a mismatch. Checksums and the app's ad hoc seal verify integrity; they do not identify a publisher. The app and CLI lack a trusted publisher signature and are not notarized.

If macOS offers an app-specific exception for the copy you verified, use **System Settings → Privacy & Security → Open Anyway** and confirm **Open**. Stop on a malware, damaged-app, or device-policy warning. Do not disable protections. See [Apple's guidance](https://support.apple.com/en-us/102445).

## 2. Install the desktop app

1. **Upgrading?** Find **Settings → Open application data folder**, quit from Manager's menu-bar menu, stop the CLI, and back up the folder. Closing the window does not quit Manager.
2. Open the DMG, drag **Stream Dock Manager.app** to **Applications**, eject the image, and open the copied app.
3. Complete **Set up permissions**. Grant access to the copy in **Applications**, not the DMG, Terminal, or a source build.
4. Choose **Check again → Connect devices**. If asked to restart, use **Quit** from the menu-bar menu and reopen the app.
5. Quit competing device managers. Choose your dock or a simulator, select a profile and page, assign an action, and choose **Save**.

| Permission                          | When needed                                                                   |
| ----------------------------------- | ----------------------------------------------------------------------------- |
| Input Monitoring                    | Required for HID input                                                        |
| Accessibility                       | Required in device setup; used by keyboard actions and automatic window rules |
| Screen Recording                    | Only for screen capture features                                              |
| Location, notifications, automation | When a feature asks                                                           |

### HID device reports “not permitted”

`hid_open_path: failed to open IOHIDDevice … (0xE00002E2)` means macOS refused device access.

1. Check **Input Monitoring** for the current app in **Applications**, even if Accessibility is already enabled.
2. After replacing an ad hoc build, remove a stale permission entry and add the current Applications copy if the existing entry no longer works. Quit and reopen Manager.
3. If it persists, note the device model, Manager build and Diagnostics output. Device-management policy may still block access.

Do not run Manager as root, disable SIP, or remove quarantine to fix permissions.

## 3. Install the CLI, if needed

Copy the CLI and notices from the DMG, or extract the portable ZIP into a folder you control. For example:

```sh
"$HOME/Applications/stream-dock-manager-cli/stream-dock-manager-cli" --version
"$HOME/Applications/stream-dock-manager-cli/stream-dock-manager-cli" --help
```

Use the path where you actually placed the executable. If macOS blocks the verified CLI, dismiss the warning, check **Privacy & Security** for its app-specific **Allow Anyway** option, and retry. This does not notarize the executable. No installer changes `PATH`.

Quit Manager before CLI commands that open its configuration, or put `--config /absolute/separate/directory` before the subcommand. Stop the CLI before replacing its files.

## Optional dependencies

| Feature                         | Requirement                                                                                                                                    |
| ------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| JavaScript plugins              | Supported Node.js LTS via **Settings → Runtimes**                                                                                              |
| MP4 import                      | FFmpeg on `PATH`, at `/opt/homebrew/bin/ffmpeg` or `/usr/local/bin/ffmpeg`, or an absolute `STREAM_DOCK_MANAGER_FFMPEG` path set before launch |
| GIF and prepared-media playback | No FFmpeg needed                                                                                                                               |

## Back up or uninstall

Both products use `~/Library/Application Support/app.streamdockmanager.Stream-Dock-Manager/` by default. **Settings → Open application data folder** shows the actual path. Back it up with both tools stopped; profile exports are not a full backup.

To remove the desktop app, quit first and move the Applications copy to Trash. To remove the CLI, delete only its copied files. Keep the separate data folder to retain your setup.
