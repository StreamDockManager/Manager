# Feature and recovery reference

[← Getting started](README.md) · [Contributor guide](CONTRIBUTING.md) · [Device support](hardware/README.md)

New to Manager? [Install it](README.md#install), then [assign your first control](README.md#set-up-your-first-control).
Use this reference when you want to configure a feature or recover a setup.

| I want to…                                      | Start here                                                                                                                                     |
| ----------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| Change what a control does or displays          | [Preview, apply and save](#preview-apply-and-save) · [Device events](#bind-actions-to-device-events) · [Dial actions](#configure-dial-actions) |
| Assign swipes on an N4 or N4 Pro                | [Device events](#bind-actions-to-device-events)                                                                                                |
| Install a plugin                                | [Plugins and Node.js](#add-plugins-and-nodejs)                                                                                                 |
| Save values or approve plugin access            | [Vault](#vault-variables--secrets)                                                                                                             |
| Switch profiles automatically                   | [Profiles and pages](#automatic-profiles-and-pages)                                                                                            |
| Record a keyboard macro                         | [Keyboard recordings](#keyboard-recordings)                                                                                                    |
| Fix damaged configuration or roll back a change | [Configuration recovery](#configuration-recovery) · [Profile versions](#profile-versions-and-plugin-recovery)                                  |
| Check storage limits or compatibility           | [Advanced reference](#advanced-reference)                                                                                                      |

## Preview, apply and save

Selecting an action or changing its appearance previews the display without running or saving the action. Previews appear in the editor and on a connected, awake dock. Plugins without live feedback use their declared artwork.

| Control                                     | Result                                                                                     |
| ------------------------------------------- | ------------------------------------------------------------------------------------------ |
| **Apply appearance**                        | Update the current binding draft                                                           |
| **Save**                                    | Persist the action and appearance                                                          |
| **Cancel** in the appearance editor         | Discard that appearance draft                                                              |
| Leave the selected control                  | Restore its saved device display                                                           |
| **Reset** in **Icon, label & live display** | Restore the action's default label, icon, colors, media and layout in the appearance draft |

Reset leaves action settings and dial/activation assignments unchanged. Cancel discards the reset; Apply appearance and then Save keep it. New Keyboard macro actions place the icon at the top, label in the center and status at the bottom.

## Add plugins and Node.js

1. Open **Plugins** and choose a catalog download, **Import plugin** for a local ZIP,
   or drop a ZIP or unpacked `.sdPlugin` folder onto the desktop editor.
2. Review the package, install it, then explicitly enable it.
3. Open **Plugin settings** if it needs a service connection.
4. Assign its action to a control and save the binding.

Folder drops require a `manifest.json` at the folder root and use the same review as ZIPs.
Use **Import plugin folder** if the webview cannot read the drop or your plugin needs hidden
files or executable permissions on helper files.

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

**Plugins → Import installed plugins** discovers VSD Craft and OpenDeck plugin folders. Select the source application before scanning. Choose a custom folder for other standalone packages, select packages, and review their permissions before importing. Manager copies packages without changing their source. Required Manager permissions must be approved before installing or replacing a package. New imports stay disabled until explicitly enabled; reviewed replacements preserve the existing enabled state. Automatic import from the installed Elgato Stream Deck application is not offered. Compatibility with supported older or unencrypted Stream Deck plugins remains available through manual folder or package import, including `.streamDeckPlugin` archives. Protected Marketplace packages remain unsupported: Manager does not decrypt manifests or bypass licensing/integrity protection.

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

### Bind actions to device events

In **Device editor**, select a key, dial, or touch zone and choose **Use Events**. Each supported event has its own action and settings. For example, one dial can adjust volume when turned, mute on a press, switch outputs on a double press, and reset volume on a triple press. Existing key, dial, and touch-zone bindings keep their current behavior unless you convert them.

Select an empty event row to add an action. Assigned rows show **Edit** and **Clear**. For keys, dials, and touch zones, choose **Use single action** to return to the standard editor; review which extra assignments would be removed, then save to apply. The standard dial keeps its left, right, and press actions.

In the desktop app, select an event and click **Test event**, then perform it on the highlighted control of a connected physical dock. The test reports whether the input matched using the current draft's assignments and timing, without running the bound action. It stops when a gesture settles, after eight seconds without a match, or when you click **Cancel test** or press Escape.

When both double and triple press are assigned, Manager waits briefly after the second click to check for a third. The default gap between completed presses is 300 ms; the default long-hold threshold is 600 ms. A recognized triple does not also trigger the single or double action. If you assign both a physical down/up event and a gesture, the physical edge action can run alongside the gesture.

Only events supported by the active device are offered. The N4 Pro knobs report clicks and rotation steps, not reliable physical releases, so they can use multi-press bindings but do not offer separate down/up, long hold, or turn-while-held. Its touch zones offer multi-tap, and the separate strip offers supported swipes. If a profile came from more capable hardware, unsupported rows remain visible and preserved but do not execute.

**Configure swipes** opens the strip in Events mode. Each direction can have its own
action. By default, left goes to the next page and right to the previous page. On
models with vertical swipes, up goes to the next profile and down to the previous
profile. The N4 offers left and right only. Clear a direction and save to disable it;
tapping a zone is configured separately.

Profiles that use Events require **Manager 0.1.1 or newer**. The exported archive includes the minimum version in `export-notes.txt`.

### VSD Craft action settings

Manager saves occasional runtime `setSettings` calls immediately. For legacy VSD Craft plugins, three calls within a second start a 5-second buffer. Every additional three attempts in a second add another 5 seconds, up to 30 seconds at 18 attempts per second. The window does not slide indefinitely: Manager batches the due changes into one workspace save. For each binding it merges object fields, with newer conflicting values winning; arrays and scalar values are replaced. The calling runtime can read its pending values with `getSettings` before they reach disk. Settings still buffered when the plugin session ends are not durable. Inspector saves and global settings are not buffered by this policy.

### HTML plugin entry points

An HTML file in `CodePath` or the applicable platform override runs as a local browser
plugin without Node.js. The host opens it for background action events when the plugin
is running. The same file can also be an action's `PropertyInspectorPath`: the separate
inspector receives an additional settings argument, while the runtime does not.
Use package-relative paths for scripts, Workers, images and audio. Local audio files can
be served without granting extra network or device permissions; playback remains subject
to the platform webview's media policies.

### Manage Node.js LTS

JavaScript program entry points such as `.js` use **Node.js LTS**. Open **Settings → Runtimes** to install a private runtime or choose an existing installation. You can select any offered release from a supported LTS line; the exact selection is retained when new releases appear. Runtime metadata checks do not automatically install or upgrade Node.js.

Expand **Node.js installation details** to delete an unused private runtime with its trash icon. Manager asks for confirmation and never deletes existing system or user-managed installations. Stop plugins using the runtime first, or select a replacement and restart them. Saved selections are retained after deletion and report the runtime missing until it is reinstalled or replaced.

Native Go, Rust, and C++ packages run their compiled executables. Packaged Python plugins include their own interpreter. Manager installers do not bundle Node.js.

</details>

## Vault: Variables & Secrets

Vault is an encrypted local store for variables, credentials, paths, and locations.
Open **Vault**, pick an unlock method, and choose **Add entry**. Use **Usage** to
see which action fields accept references. No value is shared just because you create it.

| Tab         | What you can do                                                                |
| ----------- | ------------------------------------------------------------------------------ |
| **Entries** | Add your own values, copy references, and view or change entries               |
| **Plugins** | Inspect installed plugin settings and installation-private entry metadata      |
| **Access**  | Review or revoke grants and see built-in action references that need attention |
| **Usage**   | Find supported fields and copy example references                              |

**Copy reference** copies the token; **Copy value** copies the actual value in the viewer.
Locking Vault or closing the viewer does not clear the system clipboard. An eye button
opens a personal value, not a plugin-private entry. JSON opens in an expandable tree;
Location uses an offline map. Viewing a location or file path does not send coordinates
or open, read, or share the target.

Every type can be protected or unprotected, and every entry stays encrypted. Paths,
locations, and credentials are protected by default when created. With a Vault master
password set, use the **Actions** lock icon to change protection without re-entering the
value. Unprotecting or viewing a protected value requires the current password.
With OS-only unlock, switch to password mode before revealing a protected value.
Changing protection does not grant plugin access.

<details>
<summary>Storage and refresh behavior</summary>

Vault stores values, entry metadata, and grants in encrypted SQLCipher rows. Listing
entries does not read saved values. Revisiting Vault checks its lock state and revision
before reusing the personal metadata list. **Plugins** and **Access** load on demand;
**Refresh** reloads the active view. Locking clears this cache. It never stores passwords
or entry values.

The earlier development-only Vault format is not migrated or erased automatically.
If you no longer need it, use **Reset Vault** after reading its confirmation. The
separate plugin settings keystore is unaffected.

</details>

On normal desktop startup, Manager automatically unlocks an existing OS-protected Vault
using the operating system credential store. If the store is unavailable, ordinary plugins
start and you can retry **Unlock** from the Vault page. Recovery startup skips automatic
unlock. Reopening the editor preserves a manual or idle lock. **Create with password** requires the
chosen password on every unlock, including at startup. Manager cannot recover a forgotten
password. **Change password** requires the current password and confirmation of the
new one. Skip leaves ordinary plugins usable while plugins with required Vault setup wait.
Unlocking does not replay an action. Lock hides entry and access details and stops new
Vault reads. Missing keyring access is an error, not a request to reinitialize. Manager
backups do not include the Vault; copying files is not an unlock or recovery mechanism.

### Transfer personal Vault entries

1. In **Vault → Entries**, choose **Export entries** and select the personal entries to include.
   Selections stay checked when you change pages. You can select across all pages to export
   all personal entries within the existing Vault limit of 256 entries. Password protection
   is on by default. Keep the export password separately from the `.sdmvault` file. A readable
   export leaves all selected values in plaintext.
2. If any selected entry is protected, confirm your current Vault master password. This is
   separate from the optional export password used to open the file elsewhere.
3. In the receiving Manager, unlock **Vault → Entries**. Choose **Import** or drop a single
   `.sdmvault` file onto the Entries page. If encrypted, enter the export password.
4. Review the file, use the eye button to inspect an incoming value where it conflicts, and
   select only the existing entries you want to replace. Keeping the existing value is
   the default. Confirm the import to add new entries and apply the selected replacements.

Replacements keep the existing entry's references and access approvals. Ambiguous matches,
such as multiple entries with the same name, are skipped. The export contains no plugin
settings, plugin-private entries, access grants, or contents of referenced files and folders.

Manager stores plugin settings and private SDK storage in the encrypted
`databases/datastore.sqlite3`. A separate installation key lets plugins read and update their
own data even while the personal Vault is locked or not yet created. Manager keeps profiles,
bindings, package identity, and permission decisions in `databases/manager.sqlite3`.
Unlock **Vault → Plugins** to inspect settings field names and types. **Reveal and edit JSON**
opens a selected instance, default, or global record. If the record changes while you edit it,
reopen it to review the current settings.

Disable the plugin and finish or discard open settings edits before deleting. Use the trash icon
next to a settings record to clear just that record, or the plugin’s trash icon to clear all its
settings and private SDK data. Each operation asks for confirmation. Neither removes the plugin,
its bindings, backups, or entries in the personal Vault, including entries created by plugins.
An installed plugin can create new settings after it is re-enabled. When uninstalling from **Plugins**, leave **Keep plugin settings,
private data, and bindings for reinstallation** unchecked to remove its saved settings.

Manager uses the OS credential store for the installation key. On a headless host, set `STREAM_DOCK_MANAGER_INSTALLATION_KEY_FILE` to the absolute path of a private file containing 64 hexadecimal digits generated from 32 random bytes. Keep it outside the Manager data folder and any backup directory. On Unix the file must belong to the Manager account and permit access only to its owner (for example, mode `0600`). On Windows restrict the DACL to the owner, SYSTEM and Administrators. Supply the same key on every start. If it is missing, restore the original key or regain access to the OS credential store.

Profile and macro exports omit plugin settings by default. To transfer the values, select **Include plugin settings** and enter an export password. Use that password during import. For a full database transfer including plugin settings and private SDK values, use
`data backup PATH --password-stdin` and `data restore PATH --password-stdin`, supplying the
password on standard input. Keep the password separately from the transfer. A standard Manager
database snapshot cannot restore plugin settings or private SDK values from the backup. Personal
Vault entries and direct-read grants stay local.

If startup reports that the plugin settings keystore cannot open, use **Configuration
Recovery → Plugin settings keystore**. After unlocking your OS credential store,
choose **Retry**. If the keystore file is lost or damaged, review a local encrypted
keystore backup. It still needs the original installation key and must fit the
references in the current database. If the file is missing and you don't have a
usable backup, choose **Create keystore**. If the file exists but can't be opened,
review **Reset keystore** instead. Both keep profiles, the separate personal Vault,
and the original files that are still available. The fresh datastore starts without saved plugin
settings or private SDK data. Reconfigure affected plugins; they may also need permissions
or Vault access approved again.

New automatic recovery points keep encrypted keystore copies locally. Those copies
do not include the key and are not a portable export. The regular **Recovery from
a database backup** option still restores the redacted workspace only; it does not
restore plugin settings, private SDK values, Vault entries, or grants from the workspace
snapshot. Use the separately password-encrypted CLI transfer to move saved plugin
data between installations.

A supported package can offer **Choose from Vault** during install review. Choose or
create a compatible entry and approve the displayed use. Direct secret access is optional
only where the package permits it and has a separate disclosure: the native plugin
receives the actual secret and could store or share it. Revoking access stops future
authorized reads; it cannot undo a completed disclosure. A new nonsecret Text value
created and selected in the review becomes private to that installation after successful
commit. Cancel leaves the entry in **Entries** and never starts the staged plugin.
A catalog upgrade can retain an unchanged grant when Manager verifies the repository,
publisher, release artifact, action and purpose. A local replacement, changed publisher
or broader scope requires fresh review. Unlock Vault before reviewing an upgrade that
needs its encrypted grants. Legacy packages without Vault setup continue to use their own settings.

Open **Settings → Plugin Permissions** to filter installed plugins by enabled state,
permission family, or outcome. Each row identifies the action, exact requested scope,
and purpose supplied by the plugin. Install review shows scope additions, removals and
unchanged scopes. A direct read needs both the package opt-in and approval for the
selected entry and action. For an installed package, **Review entry access** checks
the installed file inventory against the reviewed build before recording consent.
Settings shows the last successful read when Manager has recorded one.

While Vault is locked, **Revoke direct reads for this action** records a denial that
survives restart. Manager applies it to the encrypted grants before the next unlock.
Unlock to inspect or revoke a specific entry in **Vault → Access**.

Vault locks after 15 minutes without keyboard or pointer activity in its entry,
setup or permission controls. Background plugin reads leave this deadline alone.
On macOS, desktop sleep, wake, screen sleep, loss of an active session, and protected
data becoming unavailable also end the session. Windows uses suspend/resume events
and the current WTS session lock flag. Linux uses logind sleep events and the desktop's
session lock hint where those services are available. Lock detection depends on the
desktop and can lag the lock screen. Use **Lock** for immediate revocation.
Native session transitions still require qualification on each platform.

In **Vault → Entries**, use **Copy reference** to copy a token in the form
`${vault.<vault-id>.<entry-id>}`, not the saved value. Paste it alone or inline in a
supported built-in action or macro field: **Type text**, **Paste text**, **Set clipboard**;
**Show notification** or **Input prompt** title and message; **Action menu** title and
choice labels; **Write file** contents; or the URL in **Open a website**,
**Open link**, and **Search selection**. In the action editor, **Choose from Vault**
inserts at the focused field's caret or replaces selected text. **Insert run outputs**
is only for run-output placeholders and is not needed for Vault references.

**Icon, label & live display** also accepts references in authored labels, including
**Display canvas → Label text**. The editor keeps the reference, while the live canvas
and the dock show the value. The display only expands unprotected Text, Integer,
Boolean, and Location values. It clears the substituted text when the Vault locks;
unlocking or editing an entry refreshes affected displays.

Use references in **Insert date and time** formats (the value is inserted as literal text),
**Clipboard slots** names, and supported literal condition comparisons. A file-existence
condition accepts one unprotected File or Folder reference for an absolute local path.
Conditions and clipboard slots fail if a required reference is unavailable instead of
silently comparing or selecting an empty value. Commands, executable paths, destructive
file destinations, regex patterns, and permission-protected plugin settings keep their own
validation and do not inherit general substitution.

For URLs, enter a fixed HTTP(S) host and place the reference in the path, query,
or fragment (for example, `https://example.org/${vault.<vault-id>.<entry-id>}`).
Manager encodes the replacement as URL data. Path and query values are sent to the
site and can appear in browser history or server logs. Do not use a Vault reference
to set a scheme, host, port, or login in the URL.

In ordinary built-in text fields, native execution expands unprotected personal entries once.
Text, file and folder paths appear as text; Integer and Boolean use their plain
representations; JSON is compact; and Location is `latitude,longitude`, or a single
field using `.name`, `.latitude`, or `.longitude` before the closing brace. A missing,
invalid, locked, or protected reference contributes no text. API keys, bearer tokens,
and other credentials do not expand, even if unprotected. Consider where an
unprotected value will appear before putting it in a notification or clipboard.

**Reveal file** still takes one eligible file or folder reference as the entire path.
**Matches Vault JSON** remains a typed comparison. Plugin actions use their separately
reviewed permissions. Macro previews and debug runs do not read live Vault values.
The native display canvas does render eligible unprotected text. Profile and macro archives replace local Vault references with `${vault.unlinked}`; choose
or paste a local reference after importing to use a value again.

Plugins can explicitly call `format-text` when Manager advertises
`plugin-text-references-v1`. It formats only references the user approved for the
exact action's direct text or location access. Missing, invalid or unavailable
references produce no text. The formatter never returns protected values or
credentials, and it does not send the result to third parties. Plugins should save
the original reference token in settings and format it only when needed.

To save a Location, enter a place name and coordinates offline, or select **Use current
location** in the native Vault dialog. Review the coordinates and select **Save**.
Closing the dialog or locking Vault cancels the request. Manual edits discard the OS
fix's accuracy metadata.

**Reset Vault** requires typing `CLEAR VAULT ENTRIES`. It can replace a locked
Vault in either mode with an empty OS-backed Vault. It retires the old credentials,
entries and approvals. You can confirm this reset even if you have forgotten the master password.
Saved bindings retain unresolved references. After an interrupted reset, choose
**Reset Vault** again to finish. An unidentifiable database, an unexpected file
link or an unavailable protected store can require recovery before reset can finish.

The personal Vault in `databases/keystore.sqlite3` uses SQLCipher page encryption for its database and rollback journal. OS mode
protects the data key in the operating system credential store. Password mode wraps
it using Argon2id and AES-256-GCM. Switching the unlock mode re-encrypts the store with
a fresh data key and retires the previous wrapper. The same entries and references
work after switching. Previously copied files retain the protection they had when copied.
The matching `.keystore-key-<salt>.json` sidecar holds the key reference in OS mode or the
password-wrapped key in password mode. Keep it with the personal Vault database; it is separate
from the plugin datastore's installation key.

**Conditional** and **Wait for condition** can compare the JSON on the clipboard
with a whole user-owned JSON entry or a Location field. Choose **Matches Vault JSON**
in the clipboard condition. A comparison preserves the JSON type. Latitude and
longitude use two decimal places. File references and comparisons can also use
the structured form `{"vault":"${vault.<vault-id>.<entry-id>}"}`.

### Find a location

Choose **Find a location** while editing a Vault Location, creating one during package setup, or editing plugin settings that declare location support. **Use current location** requests a single position through your operating system. Review the coordinates and accuracy, then select the result to fill the draft.

By default, **Search** sends a city name or postal code and an optional country filter to [Open-Meteo Geocoding](https://open-meteo.com/en/docs/geocoding-api). Postal-code searches require a two-letter country code. No API key is needed for [noncommercial use](https://open-meteo.com/en/pricing). Location data comes from [GeoNames](https://www.geonames.org/) under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). Review the matching place names and coordinates before choosing a result. Only submitting the form sends a query. Do not enter street addresses with Open-Meteo.

To use a private or contracted Nominatim-compatible service instead, open **Search provider** and save its endpoint. Manager shares the setting across all location editors. Clear it and save to return to Open-Meteo. Manual coordinates and device positioning work without a search provider.

Operators can host the shared broker with Go from [tools/nominatim-broker](tools/nominatim-broker). Create a JSON configuration containing the upstream search URL and the address policy, for example `{"upstream":"https://geo.example.org/search","allow_addresses":true}`. From the broker directory, run `go run . -config /path/to/nominatim.json`. The default listener is `127.0.0.1:8095`. Save `http://127.0.0.1:8095/search` in Manager for a local instance, or publish a shared instance behind an HTTPS reverse proxy. The shared service enforces a single upstream request budget and caches postal responses. Public OpenStreetMap endpoints accept only postal searches through an operator-managed broker. [Nominatim usage policy](https://operations.osmfoundation.org/policies/nominatim/).

### Services requested by plugins

Open **Settings → Plugin Permissions** to review or revoke Manager services for an
installed action. The publisher supplies the purpose. Manager supplies the operation
and destination. An optional private-write approval permits a plugin to create and
rotate a credential in its own declared slot. Review and revoke private writes
separately from direct reads.

A supported dock press can open a Manager window while the editor is in the tray.
A clipboard summary previews the exact destination and editable text. **Send**
transmits the reviewed text once. **Copy** is a separate choice for the result.
A directly approved clipboard read gives the plugin the snapshot. The summary
recipe gives it a result handle. Notifications contain a generic completion message.

A Vault picker can grant an exact entry for a specific operation. Metadata browsing
has its own approval. To share a saved Location, review the plugin action’s
`vault.read-location` purpose, choose an entry and allow access. The plugin
receives its latitude and longitude at stored precision. Its name, address
and GPS accuracy stay in Vault. Change the entry or revoke direct reads in
Settings → Plugin permissions. Weather uses its own service and unit settings
for Conditions, Forecast and Sun.

Clipboard read and result Copy support macOS, Windows, X11 with XFixes, and
Wayland compositors that expose the ext or wlroots data-control protocol.
On multi-seat Wayland displays, Manager uses the seat named by `XDG_SEAT`.
If the seat is ambiguous or the compositor withholds data control, Manager reports
clipboard access as unavailable. The Wayland adapter needs a named compositor socket
that it can open for each authorized transfer. On Linux the copied text remains available after the result window
closes while Manager owns the selection. Copying something else replaces it.
Windows publishes from the foreground result window and excludes the item from
Windows clipboard history and cloud sync. All four backends enforce a 64 KiB UTF-8 limit.
Notification availability follows the native notification service and its permission
state. Physical dock, tray, native session locking and credential-store behavior
still need testing on each supported desktop.

Actions can request **Show plugin alerts and notifications**
(`notifications.create`). Official Stream Dock Manager plugins declare it as
required. Review it during installation or change your decision in **Settings →
Plugin Permissions**. Manager won’t dispatch presses or macro actions that
require this permission without approval. Approved actions can report an error, warning, success, or
information without a fresh button press. Manager identifies the originating
plugin and limits how many messages it can show. Alerts appear in the open
editor or use desktop notifications when the editor is in the background and
the OS has granted notification access. Plugins should report actionable
messages, never credentials or private provider responses.

## Automatic profiles and pages

### K1 Pro Navigation fallback

A connected K1 Pro with no compatible profile shows the six default Navigation keys without creating a saved profile. An unassigned page returns to its profile's Navigation page after 30 continuous seconds. Existing Navigation appearance customizations are preserved.

Pages with assigned bindings or custom background media are not blank. Sleep, output transitions, live previews and temporary binding layers cancel the countdown; reconnecting starts fresh. Other device models are unchanged.

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
and leaves the other turn intact. Changes remain drafts until you choose **Save**.

Each input can run a multi-action sequence. You can override either rotation direction, including individual multi-action steps; existing saved choices are preserved. Page/profile navigation wraps at the ends and skips profiles for other device models. Choose **Save** to keep all three assignments.

### Copy an assignment to other pages

To reuse a key, dial, touch region, or display assignment in the same position, choose **Save assignment to** in the inspector: **This page**, **Every page in this profile**, or **Every page in all matching profiles**. Matching profiles must use the same device model. Review the page/profile and replacement counts, confirm replacement, then save. This copies the complete binding, including appearance and all dial inputs, in one undoable transaction; unrelated controls stay unchanged. These are independent copies, not linked assignments or defaults for future pages. Absolute page targets must exist in every destination profile; use Previous/Next navigation for portable page switching.

Dial bindings use explicit independent left, right, and press inputs. Obsolete assignment formats are rejected rather than silently converted; original saved data is preserved. Press is available only on devices that report a dial press.

### Reuse a saved macro on a control

Choose the **Macro** action. Use **Edit macro** to build steps for this control,
or open **Use saved macro** and choose one from the Macros page. The binding links
to the original macro, so later edits apply everywhere it is used. Set any inputs
and choose **Save** on the binding to apply the change. To call a saved macro as
one step inside a longer sequence, add the **Run saved macro** action to that sequence.

## Keyboard recordings

Choose **Keyboard macro → Edit recording**. The action is called **Keyboard macro** in the editor; its default hardware label is **Macro**.

| Step                                       | What happens                                    |
| ------------------------------------------ | ----------------------------------------------- |
| Record in **In Manager**                   | Capture keys only in the focused recording area |
| **Stop**                                   | End recording and review the timeline           |
| **Simulate**                               | Preview timing and counts without sending keys  |
| **Apply**, then **Save** or **Save macro** | Keep the recording                              |
| **Cancel**                                 | Leave the original draft unchanged              |

Recordings store physical key positions, not text, so changing keyboard layout can change their output. Never record passwords. Saved recordings are included in workspace exports and backups.

> **macOS:** lock detection is best effort and was observed about ten seconds after locking. Stop recording or playback before locking or switching users. Native behavior still needs real-device verification.

<details>
<summary>Recording modes, platform limits and safe playback</summary>

### Capture and review

Choose **Keyboard macro** in the keyboard/text actions, then **Edit recording**. Its default hardware label is **Macro**; custom labels are preserved. Record only in the focused capture area, or add physical keys manually. **Stop** ends capture and adds releases for any keys still held; the review reports those additions. Escape is recordable while the area is focused. Use **Stop**, **Cancel**, or **Ctrl+Shift+F12** to end capture. Focus leaving the capture area stops in-app recording. Initially held keys must be released before capture starts.

**In Manager** is the default recording mode. **Another app** is shown separately and requires an available native backend and existing permissions. That mode uses a three-second countdown, a persistent status/Stop/Cancel area and its own displayed emergency shortcut (Ctrl+Shift+F11 on Windows). Changing focus does not end an explicitly selected global recording.

Stop and Cancel wait for recording cleanup without freezing the editor. Cancel remains available while Stop is pending. If native cleanup takes too long, the draft stays locked and you can retry cancellation; Manager does not claim the recorder is released or allow competing keyboard output before teardown finishes. Quit also waits for cleanup and keeps Manager open if it cannot confirm completion.

The editor preserves press/release order, overlaps, explicit repeat events and millisecond delays. Review the timeline before applying. **Apply** changes the parent action draft; **Save** or **Save macro** persists it. Canceling either editor leaves that editor's original draft unchanged. Recordings can be used in bindings, dial inputs, multi-actions and saved macros. **Simulate** previews timing and counts without sending keys. Selecting an action, importing a profile or loading saved data never starts capture or playback.

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

## Configuration recovery

Manager opens this screen when saved data blocks startup or when you start in
**Recovery mode**. Devices and plugins stay stopped while the screen is open.
Nothing changes until you save a correction or confirm a restore or deletion.

| Problem                                              | What to choose                                                                        |
| ---------------------------------------------------- | ------------------------------------------------------------------------------------- |
| One saved item is invalid                            | **Edit JSON → Validate → Save**                                                       |
| Undo a profile change                                | **Compare & versions** or the profile's **Versions** button in the editor             |
| Keep the current workspace and recover some profiles | **Recover from a database backup → Copy selected profiles**                           |
| Roll back the full workspace                         | Choose a compatible backup, then **Preview full restore**                             |
| Plugin settings keystore cannot open                 | **Plugin settings keystore**; retry, review a matching backup, or review create/reset |
| Stop without repairing                               | **Continue to Manager → Quit Application**                                            |

### Edit or compare a saved item

1. Choose **Edit JSON**. Select an issue to highlight its field or use **Find in JSON**
   (⌘F / Ctrl+F). Use Enter / Shift+Enter to move through matches.
2. Use **Compare & versions** to see saved text and, for profiles, retained undo/redo
   or backups. **Load into editor** copies a version into the draft; it does not save.
3. Choose **Validate** to check your edits, then **Save** to keep them. Manager makes
   a backup before writing and validates again. Edits clear the old validation markers.

You can edit existing profiles, macros, desktop/accessibility preferences, offline
icon and sampling choices, device power, alerts, and library organization. Some records
can also be deleted after confirmation. Removing a draft does not delete the saved
profile. Removing a profile also removes its automatic rules, navigation references,
and parameter overrides. Removing an optional preference resets it to the default.
Referenced macros must be unlinked before removal. Credentials, grants, database
metadata, deletion records, and restart flags are not editable here.

### Recover from a database backup

- **Copy selected profiles** adds only the profiles you choose. Copies get new identities;
  name collisions get a recovered suffix. Select mutually referenced profiles together.
  Missing required macros, media, or managed plugin resources block the transfer.
  Manager backs up the current database and saves the batch as one operation.
- **Preview full restore** checks and previews replacing compatible profiles, saved macros,
  preferences, plugin state, and history together. Confirm only after reviewing the
  scope and warnings. The previous database and sidecars are quarantined; the source
  backup stays unchanged. Permanently deleted media are not restored by old references.

Full restore does not recreate external media or plugin binaries. Missing required files
block restoration. Unavailable draft/history assets or external plugins are reported
separately. The standard backup is redacted: it does not restore protected plugin
settings, personal Vault entries, or grants. For protected-settings recovery see
[Vault and plugin settings](#transfer-personal-vault-entries). Incompatible or corrupt
sources are preserved; a failed restore does not fall back to ordinary startup.

### Leave recovery

- **Continue in recovery mode** opens Manager with automatic plugin startup disabled.
- **Restart normally** restarts using the repaired configuration.
- **Cancel** keeps the recovery screen open. **Quit Application** exits even if the
  other choices are unavailable. Saving alone does not leave the screen.

### Database files and backup limits

**Settings → Open application data folder** (or **Open data folder** in recovery)
shows the current configuration. Back it up with Manager and the CLI stopped.
Profile exports are not complete backups.

| Path under the data folder    | What it contains                                                              |
| ----------------------------- | ----------------------------------------------------------------------------- |
| `databases/manager.sqlite3`   | Workspace configuration                                                       |
| `databases/datastore.sqlite3` | Encrypted plugin settings and private SDK data; requires the installation key |
| `databases/keystore.sqlite3`  | Separate encrypted personal Vault                                             |
| `databases/cache.sqlite3`     | Index for recognized cached files                                             |
| `databases/recovery/`         | Backups and quarantine evidence                                               |

Earlier development database layouts are not migrated. Use a separate data folder for this build
instead of reusing the old development data folder.

Manager relocates compatible flat database files and the old `recovery/` directory
without converting their schema. It does not overwrite conflicting copies. If an
unfinished persistent SQLite transaction blocks relocation, open the data folder with
the previous Manager and quit it cleanly before retrying. Use a separate data folder
or compatible pre-move backup to run an older build. It may create a new, empty database
in the old location.

An unsupported format is not migrated or reset. If the primary database is missing,
Manager creates a fresh one and preserves pre-existing files, backups, and orphan
sidecars or recovery markers. With no recoverable backup, startup is a fresh install.
With a backup, you can review and transfer selected profiles; nothing is restored automatically.
Use a separate absolute test path if working with development builds.

The retention target is **32 files or 1 GiB**, whichever is reached first, across **all
files inside `databases/recovery/`**, including nested evidence, manual snapshots, and
unknown file types. After a new validated automatic snapshot, older files are removed
first. The new snapshot is kept even if oversized; unfinished restore defers cleanup.
Links are counted and unlinked, never followed. Exports elsewhere are unaffected.

### Share a diagnostic report

**Diagnostics → Export diagnostic report** previews the exact JSON before saving. It
contains selected build, OS, runtime, plugin, and performance summaries, not raw logs,
profiles, settings, hostnames, serial numbers, or user paths. Manager does not upload
it. Review before sharing; the preview expires after ten minutes.

## Advanced reference

<details>
<summary>Cache storage and retention</summary>

**Settings → Cache storage** shows total indexed usage and per-folder storage, entry counts, protected counts, and retention, with a confirmed **Delete cache** action. Automatic cleanup runs on a background worker at startup and every 15 minutes while Manager is running, independently of automatic update checks.

| Folder              | Contents                                        | Cleanup target                                    |
| ------------------- | ----------------------------------------------- | ------------------------------------------------- |
| `cached-media`      | Reusable plugin artwork                         | 7 days since last use; 2 GB (2,000,000,000 bytes) |
| `download-cache`    | Verified plugin downloads                       | 1 day since last use; 2 GiB; 32 entries           |
| `updates/downloads` | Manager update downloads                        | 2 days; 4 GiB; 128 entries                        |
| `updates/cache`     | Update metadata                                 | 1 day; 512 MiB (536,870,912 bytes); 64 entries    |
| `firmware-cache`    | Last accepted catalog from each firmware source | 18 MiB combined; retained offline until cleared   |

The cache index stores file sizes, last-use times, and deletion hints in
`databases/cache.sqlite3`. It imports existing entries once; files added by other
programs afterward do not appear in reported usage. Choose **Refresh cache usage**
to update the display. Cleanup removes idle entries marked for deletion, expired
entries, then the least-recently used idle entries. Active transfers, active artwork,
and package consumers are protected and may keep a folder over its target.

**Delete cache** clears only recognized idle content. It never removes manual installer
downloads, unknown legacy files, symbolic links, installed plugins/runtimes, profiles,
or settings.

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

### Network monitoring in protocol v1

Manager advertises the optional `network-demand-v1` and `network-payload-v1` capabilities with a per-run identifier. A supporting plugin opts in by including `network_demand: true` in its registration payload. While the network monitor is visible, Manager sends `requestNetworkPayload` and the SDK replies with a cumulative `networkPayload` sample. Byte counters continue accumulating while the monitor is closed. Plugins that omit the opt-in keep their existing reporting behavior. New SDKs use periodic reports with older hosts that advertise only `network-payload-v1`.
