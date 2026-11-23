# Install Stream Dock Manager on Linux

[Download Manager](https://github.com/StreamDockManager/Manager/releases/latest) · [Build from source](https://github.com/StreamDockManager/Manager#run-from-source)

| Target       | Architecture | Download suffix   |
| ------------ | ------------ | ----------------- |
| Ubuntu 24.04 | x86-64       | `linux-amd64.deb` |
| Nobara 44    | x86-64       | `linux-amd64.rpm` |

Desktop and CLI packages install separately. A target label is not a claim of native qualification.

## 1. Check the package

The packages are unsigned. Download from a source you trust; compare SHA-256 with `SHA256SUMS` and size with the matching catalog. Use the actual filename:

```sh
file="$HOME/Downloads/your-manager-package.deb"
sha256sum "$file"
wc -c < "$file"
```

Use `catalog.json` for desktop, `cli-catalog.json` for the CLI, or the supplied development catalog. Stop if bytes do not match. Checksums verify bytes, not who built them. Do not bypass security or dependency errors.

## 2. Install the desktop app

**Upgrading?** Find **Settings → Open application data folder**, use **Quit** from the tray, stop the CLI, and back up the folder before replacing packages. Closing the window does not quit Manager.

Replace `VERSION` with the downloaded version and run the command for your distribution:

```sh
# Ubuntu
sudo apt install "$HOME/Downloads/stream-dock-manager-VERSION-linux-amd64.deb"
# Nobara
sudo dnf install "$HOME/Downloads/stream-dock-manager-VERSION-linux-amd64.rpm"
```

1. Review the package manager’s dependency changes.
2. Quit other device managers, then reconnect the dock so the installed udev rules apply.
3. Launch **Stream Dock Manager** as your normal user. Choose your dock or a simulator in setup.
4. Select a profile and page, assign an action to a control, and choose **Save**.

Do not run Manager as root or make HID devices world-writable. Wayland features may ask for permission through your desktop portal; approval does not enable every [keyboard macro backend](https://github.com/StreamDockManager/Manager#keyboard-recordings).

## 3. Install the CLI, if needed

```sh
# Ubuntu
sudo apt install "$HOME/Downloads/stream-dock-manager-cli-VERSION-linux-amd64.deb"
# Nobara
sudo dnf install "$HOME/Downloads/stream-dock-manager-cli-VERSION-linux-amd64.rpm"
```

Reconnect the dock and check `stream-dock-manager-cli --version` or `stream-dock-manager-cli --help`. The package installs `/usr/bin/stream-dock-manager-cli`, notices under `/usr/share/doc/stream-dock-manager-cli/`, and separate `60-stream-dock-manager-cli.rules` access rules. It does not enable autostart or change `PATH`.

For a portable ZIP, extract it into your own folder, keep its notices, and run `./stream-dock-manager-cli` there. The ZIP does not install udev rules. Quit Manager before CLI commands that open its configuration, or pass `--config /absolute/separate/directory` before the subcommand.

## Dependencies

| Feature                         | Requirement                                                          |
| ------------------------------- | -------------------------------------------------------------------- |
| Normal installation             | Let APT or DNF resolve the native libraries                          |
| MP4 import                      | FFmpeg, required by desktop packages and recommended by CLI packages |
| GIF and prepared-media playback | No FFmpeg needed                                                     |
| JavaScript plugins              | Supported Node.js LTS via **Settings → Runtimes**                    |

## Back up or uninstall

Keep `~/.config/stream-dock-manager/`, or `$XDG_CONFIG_HOME/stream-dock-manager/` when an absolute XDG override is set. **Settings → Open application data folder** shows the actual path. Back it up while both tools are stopped; profile exports are not a full backup.

Turn off **Start in the background at login** if enabled and quit Manager before removing it.

| Remove  | Ubuntu                                    | Nobara                                    |
| ------- | ----------------------------------------- | ----------------------------------------- |
| Desktop | `sudo apt remove stream-dock-manager`     | `sudo dnf remove stream-dock-manager`     |
| CLI     | `sudo apt remove stream-dock-manager-cli` | `sudo dnf remove stream-dock-manager-cli` |

For a portable CLI, remove only the extracted files.
