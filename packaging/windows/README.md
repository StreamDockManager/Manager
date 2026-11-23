# Windows installer guide

[Download Manager](https://github.com/StreamDockManager/Manager/releases/latest) ·
[Build a Windows package](../README.md#local-target-builds)

## Install and get running

1. Follow the [Windows setup guide](../install-instructions/windows/INSTALL-INSTRUCTIONS.md)
   for the desktop EXE or optional CLI.
2. Open Manager from Start and finish setup. Then [assign your first control](../../README.md#set-up-your-first-control).

When upgrading or uninstalling, choose **Quit** in Manager's tray menu, stop the CLI,
and back up the location shown in **Settings → Open application data folder**.

## Choose what to keep when uninstalling

| Removal                                                      | Configuration                                            |
| ------------------------------------------------------------ | -------------------------------------------------------- |
| Normal uninstall, default options                            | Kept                                                     |
| Interactive uninstall with confirmed data cleanup            | Default shared Manager/CLI folder is permanently deleted |
| Upgrade, silent/passive removal, or installer-driven removal | Kept                                                     |
| Standalone CLI removal                                       | Kept                                                     |

> Back up any files you need before choosing data cleanup. The cleanup is permanent, not a move to the Recycle Bin.

## What the cleanup option includes

The desktop uses a current-user NSIS installer. To request cleanup, select the initially unchecked **Delete default Manager/CLI configuration** option, then accept the second, default-No confirmation showing the exact folder and scope.

| Location or data                                                        | Cleanup behavior                                                                    |
| ----------------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| `%APPDATA%\streamdockmanager\Stream Dock Manager\config`                | Delete its contents, including any manually copied exports or nested configurations |
| Empty application parent                                                | May be removed after cleanup                                                        |
| Custom configurations, exports and media **outside** the default folder | Retained                                                                            |
| OS credential-store entries and WebView caches                          | Retained                                                                            |
| Shared CLI configuration inside the default folder                      | Deleted too; the CLI uses the same configuration                                    |

Removing the standalone CLI never removes Manager data. Desktop cleanup is not a complete privacy wipe.

### Upgrades and unattended removals

Silent (`/S`), passive (`/P`), update (`/UPDATE`), and installer-launched removals keep configuration without offering cleanup. Installer-launched NSIS removals receive `/UPDATE`. The fallback for older Tauri installers recognizes in-place invocations by comparing `$EXEDIR` and `$INSTDIR` because NSIS consumes `_?=` before hooks run. Older installed uninstallers do not acquire this behavior until replaced.

## Safety and failure behavior

<details>
<summary>How the cleanup helper confines deletion and reports failures</summary>

The pre-uninstall hook only records consent. Actual cleanup runs after Tauri's
cancelable app-stop step and normal application removal. The installer extracts
the exact packaged executable into its private temporary directory, invoking its
internal cleanup mode before Tauri, plugins or the configuration store start.
The helper accepts no custom target. It resolves the current user's default root
through the application's OS-directory implementation, rejects redirected or
reparse-point paths, checks the complete tree and obtains the shared writer lock.
Keep Manager and CLI commands closed. Directory handles prevent redirect/rename
while the checked entries are removed. Cleanup removes only known files and
empty directories; it does not recursively follow a changed path.

A preflight or lock failure retains configuration. A failure after removal starts
can leave a partly removed folder. The installer displays the error and path,
returns a nonzero status, and does not claim that cleanup completed. No files are
scheduled for deletion after reboot. This is permanent deletion, not recycling.

</details>

## Pinned installer template

`installer.nsi` is derived from the template embedded in `@tauri-apps/cli` 2.12.6.
`template-source.json` records that unmodified template's SHA-256. The dependency is
pinned in both npm manifests; keep the MIT notice in `TAURI-LICENSE-MIT` with the
source and release notices. Project-owned behavior is in `uninstall.nsh` and
`src-tauri/src/uninstall_data.rs`. The template changes are limited to marking
installer removals, initializing/hiding the optional checkbox, detecting failed
application removal, invoking the hook functions and removing the unrelated
upstream recursive bundle-ID cleanup.

When updating Tauri CLI, compare its actual template to the recorded source and
rebase these changes before changing the pin. Recheck hook order, automatic
uninstaller relocation, command-line switches and `DeleteAppDataCheckboxState`.
Do not silently restore upstream unchecked recursive deletion.

### Native acceptance checklist

Use disposable user/data fixtures and cover:

- Unchecked, checked, declined and canceled interactive removal.
- Silent, passive, update, interactive upgrade/downgrade, same-version replacement
  and an old installer's in-place uninstaller invocation.
- A running Manager or CLI writer, absent configuration, nested files, sibling
  application data and custom configuration outside the default root.
- Root, ancestor and nested junctions/reparse points, concurrent path changes,
  read-only files, access-denied failures and report/helper failures.

Cross-compilation and NSIS compilation validate code and packaging syntax only;
they do not qualify native Windows installer UI, locking or filesystem behavior.

[Back to packaging](../README.md)
