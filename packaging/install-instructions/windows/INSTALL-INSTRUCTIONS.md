# Install Stream Dock Manager on Windows

[Download Manager](https://github.com/StreamDockManager/Manager/releases/latest) · [Build from source](https://github.com/StreamDockManager/Manager#run-from-source)

Use **Windows 11 x64** and downloads labeled `windows-amd64`. The desktop app uses an EXE installer. The optional CLI uses a separate EXE installer or portable ZIP. Keep the included notices and licenses.

## 1. Check the package

The packages are unsigned. Compare SHA-256 with `SHA256SUMS` and size with `catalog.json` (desktop), `cli-catalog.json` (CLI), or the supplied development catalog. Use your actual filename:

```powershell
$file = "$HOME\Downloads\your-manager-package.exe"
Get-FileHash -Algorithm SHA256 -LiteralPath $file
(Get-Item -LiteralPath $file).Length
```

Stop on a mismatch or an unknown source. Checksums verify bytes, not the publisher. **Smart App Control** may reject unsigned software without an exception. If blocked, stop or ask your administrator; do not disable Windows security globally. See [Microsoft's guidance](https://support.microsoft.com/en-us/windows/security/threat-malware-protection/smart-app-control-frequently-asked-questions).

## 2. Install and open the desktop app

1. **Upgrading?** Find **Settings → Open application data folder**, quit Manager from the system tray, stop the CLI, and back up the folder. Closing the editor does not quit Manager.
2. Run the desktop EXE. Use the same install location when upgrading. Uninstall an older MSI edition first, keeping application data.
3. Open **Stream Dock Manager** from Start. Setup may need internet access to download WebView2.
4. Quit other device managers. Choose your dock or a simulator in setup, select a profile and page, assign an action, and choose **Save**.

## 3. Install the CLI, if needed

Stop running CLI commands and uninstall an older CLI MSI first, if present. The EXE installs to `%LOCALAPPDATA%\Programs\stream-dock-manager-cli\` and adds its own uninstall entry. It does not install the desktop app or change `PATH`.

```powershell
& "$env:LOCALAPPDATA\Programs\stream-dock-manager-cli\stream-dock-manager-cli.exe" --version
& "$env:LOCALAPPDATA\Programs\stream-dock-manager-cli\stream-dock-manager-cli.exe" --help
```

For a portable ZIP, extract to your own folder, keep the notices, and run the EXE by full path. Quit Manager before CLI commands that open its configuration, or put `--config C:\absolute\separate\directory` before the subcommand.

## Optional dependencies

| Feature                         | Requirement                                                                         |
| ------------------------------- | ----------------------------------------------------------------------------------- |
| JavaScript plugins              | Supported Node.js LTS via **Settings → Runtimes**                                   |
| MP4 import                      | FFmpeg on `PATH` or an absolute `STREAM_DOCK_MANAGER_FFMPEG` path set before launch |
| GIF and prepared-media playback | No FFmpeg needed                                                                    |

## Back up your data

Both products use `%APPDATA%\streamdockmanager\Stream Dock Manager\config\` by default. Find the actual path in **Settings → Open application data folder**. Back it up with both tools stopped and keep the backup elsewhere. A profile export is not a full backup.

## Uninstall

Use **Settings → Apps → Installed apps**, then pick the desktop app or **Stream Dock Manager CLI**. Quit Manager from the tray and stop the CLI first.

| Removal                                                                                 | What happens to your configuration                        |
| --------------------------------------------------------------------------------------- | --------------------------------------------------------- |
| Normal desktop removal or a CLI uninstall                                               | Kept                                                      |
| Upgrade, silent/passive, or installer-driven removal                                    | Kept                                                      |
| Desktop removal with **Delete default Manager/CLI configuration** checked and confirmed | Default shared Manager and CLI folder permanently deleted |
| Portable CLI removal                                                                    | Only remove extracted files                               |

The optional desktop cleanup deletes all files in the displayed default folder, including recovery backups, logs, and files you copied there. It does not touch custom configurations, exports, or media outside that folder. Windows credential-store entries and WebView caches remain. If cleanup is blocked or fails, check the reported folder; some or all data may remain.
