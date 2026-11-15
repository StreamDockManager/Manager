# Install Stream Dock Manager on macOS

Use this guide for a desktop or CLI package you already have. Manager is **unreleased at 0.1.0**; these instructions do not mean a public download is available. To build your own copy, see the [source setup guide](https://github.com/StreamDockManager/Manager#run-from-source).

## Choose your package

| Your Mac      | Package label | Minimum system |
| ------------- | ------------- | -------------- |
| Apple Silicon | `macos-arm64` | macOS 14       |
| Intel         | `macos-amd64` | macOS 14       |

| Product        | Format              | What it provides                                   |
| -------------- | ------------------- | -------------------------------------------------- |
| Desktop app    | DMG                 | The graphical editor and background device runtime |
| Standalone CLI | DMG or portable ZIP | A command-line executable, without a GUI wrapper   |

Neither product requires the other. Keep the included license, dependency notices and instructions with the files.

## 1. Check the package

Neither product has a trusted publisher signature or notarization. The desktop app has an **ad hoc resource seal** for bundle integrity; it does not identify the publisher or remove macOS opening restrictions.

Review the package's exact source commit and build run. When checksums and catalogs accompany the build, compare SHA-256 with `SHA256SUMS` and byte size with `catalog.json` (desktop) or `cli-catalog.json` (CLI) from that same build. Development artifacts may supply a native catalog instead. Substitute your actual filename:

```sh
shasum -a 256 "$HOME/Downloads/stream-dock-manager-0.1.0-macos-arm64.dmg"
wc -c < "$HOME/Downloads/stream-dock-manager-0.1.0-macos-arm64.dmg"
```

Stop on a mismatch. If you cannot establish where the package came from, do not treat it as verified. Checksums detect changed bytes; they do not authenticate a publisher or prove binary/source equivalence. Development artifacts are not release-qualified.

If macOS blocks an unidentified developer or an app it cannot check, review the download first. To approve that specific app when offered, try opening it, then use **System Settings → Privacy & Security → Open Anyway** and confirm **Open**. Stop if macOS reports malware, a damaged app or a device-policy restriction. Do not disable system protections. See [Apple's opening guidance](https://support.apple.com/en-us/102445).

## 2. Install the desktop app

1. **Before upgrading:** open **Settings → Open application data folder**, choose **Quit** from Manager's menu-bar menu, and back up that folder while Manager and the CLI are stopped. Closing the editor alone leaves Manager running.
2. Open the desktop DMG and drag **Stream Dock Manager.app** to **Applications**. Replace the old app when upgrading.
3. Eject the disk image and open the copy in **Applications**.
4. Complete **Set up permissions**, then the first-run setup wizard.

### Permissions at first launch

| Permission                                 | Purpose                                      | Required to connect devices? |
| ------------------------------------------ | -------------------------------------------- | ---------------------------- |
| Input Monitoring                           | HID input                                    | Yes                          |
| Accessibility                              | Keyboard actions and automatic window rules  | Yes                          |
| Screen Recording                           | Features that need screen capture            | No                           |
| Location, notifications and app automation | Requested by individual features when needed | No blanket grant required    |

Use each permission row's **Open Settings** button and select the installed Applications copy, not the DMG, a development build or Terminal. Return to Manager, choose **Check again**, then **Connect devices**. Device discovery stays paused until both required permissions are enabled. If macOS requests a restart, fully quit from Manager's menu-bar menu and reopen it.

Quit competing device managers before connecting the dock. Choose a device or simulator in setup, select a profile/page, assign an action to a control, and choose **Save binding**.

### HID device reports “not permitted”

`hid_open_path: failed to open IOHIDDevice … (0xE00002E2)` means macOS refused the device open.

1. Check **Input Monitoring**, even if Accessibility is already enabled.
2. Confirm the permission applies to the current **Applications** copy. A source build or Terminal permission may not apply to the installed app.
3. After replacing an ad hoc build, an old enabled entry may no longer work. Remove the stale app entry and add the current Applications copy, then fully quit and reopen Manager.
4. If the error remains, collect the exact device model, app build and Diagnostics output. Device-policy restrictions can also deny access.

Do not run Manager as root, disable SIP or remove quarantine as a device-permission fix.

## 3. Install the CLI, if needed

The CLI DMG contains `stream-dock-manager-cli`, this guide, a README and notices. Copy those files into a folder you control, such as `~/Applications/stream-dock-manager-cli/`, then eject the image. Alternatively, extract the CLI ZIP into that folder.

Run the copied executable in Terminal:

```sh
"$HOME/Applications/stream-dock-manager-cli/stream-dock-manager-cli" --version
"$HOME/Applications/stream-dock-manager-cli/stream-dock-manager-cli" --help
```

If macOS blocks it, dismiss the warning with **Done** and review the exact package before approving it. **System Settings → Privacy & Security** may offer **Allow Anyway** for the standalone executable. Select that app-specific option, rerun the command and confirm **Open Anyway** if prompted. A fresh copy may prompt again. This does not authenticate or notarize the executable. Do not disable protections or remove quarantine to bypass the warning.

No installer changes `PATH`; adding the chosen folder is optional. Quit Manager before CLI commands that open its configuration, or place `--config /absolute/separate/directory` before the subcommand. Stop running CLI commands before replacing its files.

## Optional dependencies

| Feature                                  | Requirement                                                                                                                                                      |
| ---------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| JavaScript plugins                       | Supported Node.js LTS, managed separately in **Settings → Runtimes**                                                                                             |
| MP4 import                               | External FFmpeg on `PATH`, at `/opt/homebrew/bin/ffmpeg` or `/usr/local/bin/ffmpeg`, or selected by an absolute `STREAM_DOCK_MANAGER_FFMPEG` value before launch |
| GIF decoding and prepared-media playback | No FFmpeg required                                                                                                                                               |

## Back up or uninstall

Both products use `~/Library/Application Support/app.streamdockmanager.Stream-Dock-Manager/` by default. **Settings → Open application data folder** shows the actual location. It contains profiles, plugins/settings, media, drafts, history and private runtimes. Back it up while both tools are stopped; profile exports are not a complete local-data backup.

| Remove      | How                                                        | Data to keep                                           |
| ----------- | ---------------------------------------------------------- | ------------------------------------------------------ |
| Desktop app | Quit from the menu-bar menu, then move the app to Trash    | The application data folder                            |
| CLI         | Remove only the copied CLI files, then its folder if empty | Unrelated files and the shared application data folder |

See [Apple's uninstall guide](https://support.apple.com/en-us/102610) for Finder removal.
