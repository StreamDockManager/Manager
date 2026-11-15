# Install Stream Dock Manager on Windows

Use this guide for a package you already have. Manager is **unreleased at 0.1.0**; a public installer is not assumed. To build your own copy, see the [source setup guide](https://github.com/StreamDockManager/Manager#run-from-source).

## Choose your package

Use **Windows 11 x64** and packages labeled `windows-amd64`.

| Product        | Format                        | What it provides                               |
| -------------- | ----------------------------- | ---------------------------------------------- |
| Desktop app    | EXE installer                 | Graphical editor and background device runtime |
| Standalone CLI | EXE installer or portable ZIP | Command-line tools, installed separately       |

You do not need the CLI to run the desktop app. Keep included licenses, notices and instructions.

## 1. Check the package

Both products are unsigned and have no detached signatures. Review the package's exact source commit and build run before opening it.

When checksums and catalogs accompany the build, compare SHA-256 with `SHA256SUMS` and byte size with `catalog.json` (desktop) or `cli-catalog.json` (CLI) from that same build. Development artifacts may supply a native catalog instead. In PowerShell, substitute the actual filename:

```powershell
Get-FileHash -Algorithm SHA256 -LiteralPath "$HOME\Downloads\stream-dock-manager-0.1.0-windows-amd64.exe"
(Get-Item -LiteralPath "$HOME\Downloads\stream-dock-manager-0.1.0-windows-amd64.exe").Length
```

Stop on a mismatch; hexadecimal letter case does not matter. If you cannot establish the package's origin, do not treat it as verified. [Get-FileHash](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.utility/get-filehash) detects byte differences, not publisher identity or binary/source equivalence. Development artifacts are not release-qualified.

Review Windows security prompts before proceeding. **Smart App Control may block unsigned software and has no individual-app exception.** If blocked, stop or consult your administrator. Do not disable Windows security globally. See [Microsoft's Smart App Control guidance](https://support.microsoft.com/en-us/windows/security/threat-malware-protection/smart-app-control-frequently-asked-questions).

## 2. Install and open the desktop app

1. **Before upgrading:** open **Settings → Open application data folder**, choose **Quit** from Manager's system-tray menu, stop CLI commands and back up that folder. Closing the editor alone leaves Manager running.
2. Open the desktop EXE and follow the wizard. Keep the same installation location when upgrading. If an older MSI edition is installed, uninstall it first and retain application data.
3. Launch **Stream Dock Manager** from Start. Installation may download WebView2, so internet access can be required.
4. Quit competing device managers, complete setup and choose your dock or a simulator.
5. Select a profile/page, assign an action to a control, and choose **Save binding**.

Use **Quit** from the tray menu when you want to stop Manager completely.

## 3. Install the CLI, if needed

The CLI EXE installs for your user into `%LOCALAPPDATA%\Programs\stream-dock-manager-cli\` and creates its own **Stream Dock Manager CLI** uninstall entry. It does not install the desktop app or change `PATH`.

If an older CLI MSI is installed, uninstall it before running the EXE. Stop CLI commands before upgrading. Then run in PowerShell:

```powershell
& "$env:LOCALAPPDATA\Programs\stream-dock-manager-cli\stream-dock-manager-cli.exe" --version
& "$env:LOCALAPPDATA\Programs\stream-dock-manager-cli\stream-dock-manager-cli.exe" --help
```

For the portable ZIP, extract the files to a separate folder you control, retain notices/instructions and run the executable by its full path. Adding that folder to your user `PATH` is optional.

Quit Manager before CLI commands that open its configuration, or put `--config C:\absolute\separate\directory` before the subcommand.

## Optional dependencies

| Feature                                  | Requirement                                                                                             |
| ---------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| JavaScript plugins                       | Supported Node.js LTS, managed separately in **Settings → Runtimes**                                    |
| MP4 import                               | External FFmpeg on `PATH`, or an absolute `STREAM_DOCK_MANAGER_FFMPEG` selection supplied before launch |
| GIF decoding and prepared-media playback | No FFmpeg required                                                                                      |

## Back up your data

Both products use `%APPDATA%\streamdockmanager\Stream Dock Manager\config\` by default. **Settings → Open application data folder** shows the actual location. Keep this folder to retain profiles, plugins/settings, media, drafts, history and private runtimes.

Back it up while both tools are stopped. Profile exports are not a complete local-data backup. Keep backups **outside** any folder you plan to remove.

## Uninstall

Quit Manager from the tray and stop CLI commands first.

| Product or removal mode                   | How to remove it                                                       | Configuration                                                       |
| ----------------------------------------- | ---------------------------------------------------------------------- | ------------------------------------------------------------------- |
| Desktop, default uninstall                | **Settings → Apps → Installed apps → Stream Dock Manager → Uninstall** | Kept when **Delete default Manager/CLI configuration** is unchecked |
| Desktop, explicit cleanup                 | Select that checkbox and confirm the displayed folder and warning      | Default shared configuration is permanently deleted                 |
| Desktop upgrade or silent/passive removal | Installer-managed removal                                              | Kept                                                                |
| CLI EXE                                   | **Installed apps → Stream Dock Manager CLI → Uninstall**               | Kept                                                                |
| CLI ZIP                                   | Remove only extracted files and the folder if empty                    | Kept separately                                                     |

> Optional desktop cleanup also deletes the configuration used by the separately installed CLI. This is permanent deletion, not a move to the Recycle Bin.

Cleanup includes every file inside the default folder, including logs, recovery backups and manually copied files. Custom `--config` folders, exports and linked media **outside** it are untouched. OS credential-store entries and WebView caches remain; this is not a complete credential/privacy wipe.

Older installed uninstallers may not offer this scoped option. Cleanup refuses unsafe redirected folders or an active configuration writer. If cleanup fails, the application is removed but some or all data may remain; the uninstaller displays the error and folder to check.

See [Microsoft's uninstall guide](https://support.microsoft.com/en-us/windows/uninstall-or-remove-apps-and-programs-in-windows-4b55f974-2cc6-2d2b-d092-5905080eaf98).
