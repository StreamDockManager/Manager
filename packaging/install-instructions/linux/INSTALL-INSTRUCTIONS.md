# Install Stream Dock Manager on Linux

Use this guide for a package you already have. Manager is **unreleased at 0.1.0**; a public download is not assumed. To build your own copy, see the [source setup guide](https://github.com/StreamDockManager/Manager#run-from-source).

## Choose your package

| Target       | Architecture | Package           |
| ------------ | ------------ | ----------------- |
| Ubuntu 24.04 | x86-64       | `linux-amd64.deb` |
| Nobara 44    | x86-64       | `linux-amd64.rpm` |

Desktop and CLI packages are separate; neither requires the other. The CLI also has a portable ZIP. A package's target label is not proof that it has passed native desktop/device tests. Review its build and qualification notes before using it on your distribution.

## 1. Check the package

Both products are unsigned and have no detached signatures. Review the package's source commit and build run.

When checksums and catalogs accompany the build, compare SHA-256 with `SHA256SUMS` and byte size with `catalog.json` (desktop) or `cli-catalog.json` (CLI) from that same build. Development artifacts may supply a native catalog instead. Substitute your actual filename:

```sh
sha256sum "$HOME/Downloads/stream-dock-manager-0.1.0-linux-amd64.deb"
wc -c < "$HOME/Downloads/stream-dock-manager-0.1.0-linux-amd64.deb"
```

Stop on a mismatch. If you cannot establish the package's origin, do not treat it as verified. Checksums detect changed bytes, not publisher identity or binary/source equivalence. Development artifacts are not release-qualified. Do not bypass dependency or signature-policy errors to force installation.

## 2. Install the desktop app

**Before upgrading:** open **Settings → Open application data folder**, choose **Quit** from the tray, stop CLI commands and back up that folder. Closing the editor alone leaves Manager running.

Install the local file with your distribution's package manager:

```sh
# Ubuntu
sudo apt install "$HOME/Downloads/stream-dock-manager-0.1.0-linux-amd64.deb"
```

```sh
# Nobara
sudo dnf install "$HOME/Downloads/stream-dock-manager-0.1.0-linux-amd64.rpm"
```

Review dependency changes before accepting. You do not need to add a Manager software repository. See [Ubuntu package management](https://ubuntu.com/server/docs/how-to/software/package-management/) and [DNF's install command](https://dnf.readthedocs.io/en/stable/command_ref.html#install-command).

### First launch

1. Quit other device managers and reconnect the USB dock after installation so the udev access rules take effect.
2. Open **Stream Dock Manager** from the application menu as your normal desktop user.
3. Complete setup, choosing the dock or a simulator.
4. Select a profile/page, assign an action and choose **Save binding**.

Do not run Manager as root or make HID devices world-writable. Wayland actions that use desktop portals require the portal's permission flow; that permission alone does not enable every keyboard-macro backend. Consult the [keyboard platform limits](https://github.com/StreamDockManager/Manager#keyboard-recordings).

## 3. Install the CLI, if needed

Install the CLI package independently:

```sh
# Ubuntu
sudo apt install "$HOME/Downloads/stream-dock-manager-cli-0.1.0-linux-amd64.deb"
```

```sh
# Nobara
sudo dnf install "$HOME/Downloads/stream-dock-manager-cli-0.1.0-linux-amd64.rpm"
```

Reconnect the device after installing its access rules, then check the CLI:

```sh
stream-dock-manager-cli --version
stream-dock-manager-cli --help
```

| CLI package file                          | Purpose                           |
| ----------------------------------------- | --------------------------------- |
| `/usr/bin/stream-dock-manager-cli`        | Executable                        |
| `/usr/share/doc/stream-dock-manager-cli/` | Instructions and notices          |
| `60-stream-dock-manager-cli.rules`        | Separate udev device-access rules |

The package is named `stream-dock-manager-cli`. It does not install the desktop app, enable autostart or edit `PATH`.

For the portable ZIP, extract it into a folder you control and run `./stream-dock-manager-cli` there. Keep notices/instructions. Adding the directory to `PATH` is optional. **The ZIP does not install device-access rules**; use a native package when you need that setup.

Quit Manager before CLI commands that open its configuration, or put `--config /absolute/separate/directory` before the subcommand. Stop running CLI commands before upgrading or replacing portable files.

## Dependencies

| Feature                                  | Requirement                                                          |
| ---------------------------------------- | -------------------------------------------------------------------- |
| Native runtime libraries                 | Let APT/DNF resolve the package's declared dependencies              |
| MP4 import                               | FFmpeg; desktop packages require it and CLI packages recommend it    |
| GIF decoding and prepared-media playback | No FFmpeg required                                                   |
| JavaScript plugins                       | Supported Node.js LTS, managed separately in **Settings → Runtimes** |

## Back up or uninstall

Keep `~/.config/stream-dock-manager/`, or `$XDG_CONFIG_HOME/stream-dock-manager/` when an absolute XDG override is set, to retain profiles, plugins/settings, media, drafts, history and private runtimes. **Settings → Open application data folder** shows the actual location. Back it up while both tools are stopped; profile exports are not a complete local-data backup.

Before desktop removal, turn off **Start in the background at login** if enabled and quit from the tray.

| Remove      | Ubuntu                                    | Nobara                                    |
| ----------- | ----------------------------------------- | ----------------------------------------- |
| Desktop app | `sudo apt remove stream-dock-manager`     | `sudo dnf remove stream-dock-manager`     |
| CLI package | `sudo apt remove stream-dock-manager-cli` | `sudo dnf remove stream-dock-manager-cli` |

Review the removal transaction before accepting it. For a portable CLI, delete only its extracted files. Keep the separate application data folder if you want to retain your setup.
