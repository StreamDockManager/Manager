# Contributing to Stream Dock Manager

Thanks for helping improve Manager. Start with a small, focused change, run the checks that cover it, and describe what you verified. This repository builds independently and contains the desktop app, CLI, device modules and plugin host.

**Current project version: unreleased 0.1.0.** Do not change release versions or turn incomplete platform coverage into a support claim as part of an unrelated change.

| Task                          | Guide                                                 |
| ----------------------------- | ----------------------------------------------------- |
| Set up a development checkout | [Get running](#get-running)                           |
| Choose the right checks       | [Validate your change](#validate-your-change)         |
| Change the interface          | [Frontend conventions](#frontend-conventions)         |
| Change saved data             | [Managed persistence](#managed-persistence)           |
| Work on a device adapter      | [Hardware contribution guide](hardware/README.md)     |
| Build installers              | [Packaging](packaging/README.md)                      |
| Maintain CI or releases       | [CI and release reference](#ci-and-release-reference) |

## Get running

Install the [toolchain and native prerequisites](README.md#requirements): Node 26.x, npm 12.2.2, Rust 1.95+, Git, GNU Make and Python 3.11+. Rustfmt and Clippy are needed for contribution checks.

```sh
git clone https://github.com/StreamDockManager/Manager.git StreamDockManager
cd StreamDockManager
make deps
make desktop
```

For an existing checkout, start with `make deps`. The desktop command compiles Rust and launches the frontend with hot reload. Without GNU Make, `npm ci` followed by `npm run desktop` starts the same development app. For a browser-only simulator, use `npm run dev` and open the address Vite prints. Browser preview cannot test USB devices or native actions.

### Public website demo

`npm run build:public-preview` builds the existing browser simulator into
`dist-public-preview/`, served at `/preview/app/`. It does not build or start Rust,
attach devices, execute native actions, or run plugins. Sample workspace edits and
preferences are memory-only and reset when the document reloads.

Embed this build in an iframe with `sandbox="allow-scripts"` only. Serve its static
assets with `Access-Control-Allow-Origin: *` (no credential allowance), JavaScript
MIME types, and no authentication endpoints beneath `/preview/app/`. The page's CSP
and public-demo guard restrict requests to that static prefix plus local media,
omit credentials, reject redirects, and allow only same-document fragment navigation (including sidebar routes and
skip links). External links, downloads, popups, forms and file picking are disabled.
Copied ACL templates use `.template.txt` rather than `.html` so static-host clean-URL
redirects cannot break their credential-free, redirect-rejecting fetches. The generated
route manifests and revision identifiers match those copied templates.
The demo must not share native build outputs or expose a local Manager server.

Run `node --test scripts/public-preview.test.mjs` for isolation guard regressions,
and verify actual sandboxed rendering and direct-route hosting in the website.
Browser simulations are not hardware or operating-system qualification.

Use a fresh configuration for native development. Quit any running Manager first, then substitute a new absolute directory outside build/artifact folders:

```sh
npm run desktop -- -- -- --config /absolute/path/to/test-config
```

Keep real profiles and backups separate. Closing the editor does not stop the runtime; choose **Quit** from its tray/menu-bar menu.

### Prepare patched Rust dependencies

Run `python3 scripts/prepare_patched_deps.py` before direct Cargo commands, including
`cargo metadata`, `cargo fmt`, tests and xtask. This downloads pinned crate archives,
verifies their SHA-256 checksums, applies the reviewed files in `patches/`, and verifies
reusable generated sources under ignored `vendor/.cache/`. Make/npm/Tauri launchers and
CI prepare these dependencies automatically. Raw Cargo cannot do so through `build.rs`,
because Cargo resolves local dependency paths first.
Run the preparer before opening a fresh checkout in a Rust IDE as well; language
servers such as rust-analyzer invoke Cargo metadata to load the workspace. If the
IDE was opened first, prepare the dependencies and reload its Rust workspace.

For an offline build, warm the cache once, then run the preparer with `--offline` and
Cargo with `--offline --locked`. `CARGO_NET_OFFLINE=true` also disables preparation
network access. `--check` validates the prepared tree without preparing missing inputs.
Deleting the cache requires preparation again; never commit or hand-edit its contents.
See [dependency provenance](vendor/README.md) and `vendor/dependencies.json` for pins.
On Windows, use `python` instead of `python3` if that is your installed launcher.

### Linux system libraries

For the Ubuntu 24.04 build baseline, the checked-in CI installs these native packages:

```sh
sudo apt update
sudo apt install build-essential pkg-config libssl-dev libudev-dev libxdo-dev \
  libdbus-1-dev libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf ffmpeg
```

Review the package-manager transaction before accepting it. Use the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for your distribution. Installing libraries or compiling on a host does not qualify that host's desktop session or device behavior.

### Build configuration

The Make commands use [`scripts/build.py`](scripts/build.py) and public defaults in [`workspace.toml`](workspace.toml). Direct npm/Cargo commands do not load that file.

| Setting                                          | Purpose                                                      |
| ------------------------------------------------ | ------------------------------------------------------------ |
| `WORKSPACE_CONFIG`                               | Select another build-defaults TOML file                      |
| `STREAM_DOCK_MANAGER_RELEASE_REPOSITORY`         | Override the Manager release repository (`owner/repository`) |
| `STREAM_DOCK_MANAGER_PLUGINS_RELEASE_REPOSITORY` | Override the Plugins release repository (`owner/repository`) |
| `STREAM_DOCK_MANAGER_PLUGINS_CATALOG_URL`        | Override the complete public HTTPS catalog URL               |
| `WORKSPACE_JOBS`                                 | Bound build parallelism, from 1 to 128                       |

Explicit build environment values take precedence. `make help` lists the local entry points; target and installer choices are in [Packaging](packaging/README.md).

### Local macOS build consistency

Cargo defaults `MACOSX_DEPLOYMENT_TARGET` to `14.0`, matching the desktop bundle's
minimum macOS version. This also applies to C and Objective-C dependencies when
running Cargo directly. Keep `.cargo/config.toml` and `src-tauri/tauri.conf.json`
in sync when changing the supported macOS floor. An explicitly exported deployment
target overrides the default; use a consistent value for all build steps.

Development builds disable Rust incremental object reuse, matching CI, to avoid
anonymous LLVM-symbol linker failures seen with optimized local crates on macOS.
Unchanged crates still use Cargo's normal build cache. Do not set
`CARGO_INCREMENTAL=1` when reproducing or verifying this build path. A normal
`cargo build --locked -p stream-dock-manager` rebuilds affected artifacts after
these configuration changes; deleting the entire target directory is unnecessary.

## Validate your change

Run checks from the repository root. Full native checks need the platform prerequisites above.

| Change                                 | Checks and review                                                                                                                                                                                           |
| -------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Markdown only                          | Format touched files with `node_modules/.bin/prettier --write README.md` (substitute your changed files), verify links and commands, then inspect `git diff --check` and the diff; no app rebuild is needed |
| Frontend                               | `npm run build`, then `npm run preview`; inspect affected routes, dialogs and assets at desktop and narrow widths                                                                                           |
| Rust behavior                          | Run the affected crates' tests; `make test` runs workspace Rust tests                                                                                                                                       |
| Hardware descriptors or shared framing | `cargo run -p xtask -- hardware check`, affected model tests, and all dependent model tests for shared protocol changes                                                                                     |
| Desktop command or DTO                 | `cargo run -p xtask -- types`, then `cargo run -p xtask -- types --check`; commit the generated contracts with their source change                                                                          |
| Plugin connection policy               | `python3 scripts/check-connection-contract.py` and the [contract requirements](#plugin-connection-contracts)                                                                                                |
| Installers                             | Follow the [platform package checks](packaging/README.md#first-launch-checks); compilation alone does not qualify installation                                                                              |

Before submitting code changes:

```sh
npm run format
make check
make test
npm run format:check
git diff --check
```

`make check-syntax` checks Python syntax, TypeScript types, and Rust workspace/all-target compilation. It fails on Rust warnings from this checkout, including locally patched dependencies, while leaving registry dependencies under their normal lint policy. Cargo diagnostics replayed from cache are checked too. It does not modify source or install a linter. Run it on Linux to cover Linux-gated code; a Mac pass cannot validate Linux-only branches. For Rust-only checks use `python3 scripts/check_syntax.py --rust-only`.

`make check` includes this gate, validates generated types, builds the frontend, and checks hardware descriptors. It is not a substitute for tests. Product CI also checks syntax, formatting, static analysis, compilation and packaging.

Formatting can affect more files than your change. Inspect the diff and keep unrelated work intact. If you cannot run a check, report the exact command and blocker rather than calling it passed.

### Pull request checklist

- Explain the problem, the intended behavior and the files or components affected.
- Keep visual changes separate from persistence, plugin execution or device behavior unless the change needs both.
- Include screenshots for UI changes and the relevant test results.
- Record the actual OS, architecture and build identity for platform-specific checks.
- Include protocol provenance and sanitized fixtures for hardware work. Never label synthetic/replayed results as physical verification.
- Preserve project and third-party licenses and notices. Do not commit credentials, serial numbers, personal paths, plugin secrets, proprietary binaries or bulky captures.
- Keep generated output changes tied to their source and generator. Do not hand-edit generated contracts or dependency directories.

## Frontend conventions

Keep first-party templates script-free, register named Alpine controllers in TypeScript, use the CSP build, and package dependencies locally. Never load plugin HTML into first-party ACL components. Profiles and assets must be written by Rust, with validation and atomic replacement.

### IPC commands and generated contracts

Desktop IPC handlers marked `#[tauri::command]` are discovered from Rust source. The native build
derives handler registration and Tauri permission declarations from those functions and rejects
commands without an explicit decision in the window capabilities. Keep plugin views restricted.
After changing a command or DTO, run `cargo run -p xtask -- types`; check all generated contracts
without rewriting them with `cargo run -p xtask -- types --check`. The generated command argument
and result types are also used by the frontend bridge. Do not add a parallel command-name list.

### Bundled assets and ACL routes

Production builds select ACL modules through its public dependency/preload API and the application's imported entries, routed components, template features and imperative startup features. `scripts/ui-bundle.ts` prunes copied output after ACL generation: unused modules/import-map entries, component sidecars, obsolete route revisions and the build-only global manifest. Active route indexes/manifests, templates, dynamic icons and license notices remain packaged. Add the matching capability there when activating another imperative ACL feature. Keep source assets in `public/`; never prune dependencies or generated source files by hand. Run `npm run build` here, then use `npm run preview` to verify affected routes, dialogs and bundled assets when changing this selection.

Vite clears the generated index and shards in `public/acl-routes/` before ACL regenerates them
at dev startup and on each production build. Use `npm run acl:routes` to regenerate and prune that
directory from the current component manifest without building the app. Commit the updated route
index and the shard additions and deletions alongside the source changes that generated them.

### Preview the startup splash

For temporary local visual review, run `VITE_SPLASH_PREVIEW_MS=8000 npm run dev`
(or `VITE_SPLASH_PREVIEW_MS=8000 npm run desktop` for the native development app).
This keeps the splash visible for at least eight seconds from frontend startup while
normal initialization continues underneath. The opt-in is capped at 30 seconds,
ends on page navigation/close, and never delays errors. Stop the development server
and restart without the variable to disable it. Production builds ignore this setting.

## Managed persistence

Use disposable fresh configuration roots for SQLite development. Only the current schema and owned-record contract is supported: no old SQLite or JSON migration, default-on-error loading, or automatic reset. Startup and CLI maintenance validate a recovered disposable copy before opening original data for writes; backups and restore share the same pure validators. Preserve rejected primary files, WAL/SHM/journal sidecars, and recovery markers. Choose a new empty `--config` directory instead of changing user data. Run `cargo test -p stream-dock-manager-storage -p stream-dock-manager-runtime -p stream-dock-manager-plugin-host -p stream-dock-manager-plugin-runtimes -p stream-dock-manager-cli` and regenerate command contracts with `cargo run -p xtask -- types` after API changes. Tests marked for disposable loopback hosts require local socket access; inspect each ignore reason before running. Keep SQL in Rust-owned services and register pure domain validators for backups/restoration.

Explicit Recovery mode and desktop domain-validation failure open a separate configuration-recovery
window. Startup routing inspection is read-only; validated exit choices persist one-shot intent
that is consumed only after successful runtime construction. Continuing in recovery skips this
landing once and keeps automatic plugin startup disabled. Normal restart overrides a recovery
launch flag once. Native close and application quit must protect unsaved JSON without trapping
the user. It must
not create a fallback runtime, execute plugins, connect devices, or reset records. Inspection and
Validate operate on disposable snapshots while holding the configuration writer lock. Only an
explicit Save or confirmed Delete may commit a repair, after a pre-edit backup and stale-state
check. Recovery JSON is an editing representation, not a replacement persistence format. Keep
current schema checks and distinguish invalid profiles, unsaved drafts, and noneditable structural
errors. Test these paths using disposable configurations, never the developer's real database.

<details>
<summary>Storage schema, callers and preservation boundaries</summary>

### Current persistence contract and call inventory

The canonical schema is built by `crates/storage/src/schema.rs::initialize` and checked against the complete `sqlite_schema` inventory, including indexes, SQL constraints, application ID, schema v1, and `database_metadata.contract = current-v1`. Only a pristine root can initialize a new primary. The 16 JSON domains in `records.rs::Domain::ALL`, metadata, and the three plugin-storage namespace/record/receipt tables are current. The unused second context authority (`plugin_contexts`) has been removed; plugin context settings belong to workspace actions.

| Owner and callers                                                                                                     | Current contract                                                                   | Disposition                                                                                                                                                                                                                                                       |
| --------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Runtime startup, `ProfileStore::open`, CLI data maintenance                                                           | `Storage::open_validated` plus `application_data::validate_snapshot`               | Recover a disposable primary/sidecar copy, validate schema and all domain validators, then open the original. No migration or reset.                                                                                                                              |
| Workspace navigation, keyboard, draft editor, parameters, plugin prompts/policy, library organization, import recipes | `LocalPreference` with `read_preference` / `write_preference`                      | Replaced filename-shaped `try_local` / `save_local` adapters. Current SQL keys: `sessions`, `drafts`, `parameter-overrides`, `plugin-profile-prompts`, `plugin-update-policies`, `library-organization`, `import-repair-recipes`, `vendor-profile-installations`. |
| Power policy and resource alerts                                                                                      | Bounded typed domain readers/writers                                               | Current SQL keys `device-power` and `resource-alerts`; no old `profiles/*.json` key aliases.                                                                                                                                                                      |
| Built-in values, updates, plugin launch recovery, restart recovery                                                    | `builtin-values`, `updates`, `plugin-startup-attempts-v1`, `restart-recovery-once` | Retained live service records. Unknown local preference keys are rejected.                                                                                                                                                                                        |
| Desktop/application preferences, runtime preferences, logs, media deletion                                            | Closed fixed-key inventories checked by the shared validator                       | Unknown/obsolete keys are rejected rather than ignored. Plugin-owned settings JSON remains opaque within supported vendor contracts.                                                                                                                              |
| Profiles/macros, full or delta history, drafts, import/export                                                         | Current typed decoding and domain validation                                       | Required fields cannot become defaults. Optional fields deliberately omitted by current writers remain optional. Both current history encodings remain supported with integrity checks.                                                                           |
| Sensitive parameter overrides                                                                                         | OS credential references                                                           | Plaintext migration endpoint removed. Unsupported plaintext is rejected without printing secrets or writing the source. Use a fresh configuration and re-enter secrets.                                                                                           |
| Snapshot creation, explicit restore, recovery reconciliation                                                          | Same schema/domain checks; disposable snapshot opening                             | Preserve backup retention, raw prior evidence, deletion reconciliation, recovery fencing, quotas, generations, receipts, and writer exclusion. Unsupported candidates never replace the primary.                                                                  |

`ProfileStore::local` remains a test-only convenience, not a production error fallback. The old filename detector in `schema.rs` only prevents overwriting unsupported directories. Neither is a migration route. Schema and preservation regressions live in `crates/storage/src/tests.rs`; current payload/key/dial/draft regressions live in `crates/runtime/src/profile_database.rs`. No startup-speed or allocation improvement is claimed without measurement.

The configuration writer lock excludes cooperating Manager processes. Preflight fingerprints primary and sidecar contents before/after copying and again before opening the source, then reruns domain validation before journal configuration. Close external SQLite editors/writers first: a process that ignores the root lock can still race the final fingerprint/open boundary. Preservation guarantees do not cover arbitrary concurrent outside writes.

</details>

## Plugin connection contracts

Edit `crates/plugin-host/connection-contract.v1.json` for static connection policy. Its checked-in JSON Schema and native build validation generate typed constants used by registration and admission. No runtime config reload or user-editable capability override exists. Keep session IDs, permissions, OS/locale/theme/device state and package-version derivation outside the file.

The separate `connection-implementations.v1.json` registry bounds declarations with concrete implementation symbols and exact executable tests. Adding a config capability or increasing vendor support versions without corresponding implementation evidence fails the build. Run `python3 scripts/check-connection-contract.py` to build and execute every selected conformance test; nonexistent, empty or ignored selections fail. CI runs the same gate. The pinned vendor source inventory and compatibility matrix document qualification evidence, not runtime policy; they must still pass `scripts/vendor-compatibility.py`, and full qualification still requires every mandatory row to be verified.

## Keyboard macro qualification

Run `cargo test -p stream-dock-manager-runtime keyboard_macro` for the portable contract,
recorder normalization, fake-clock playback and fault-injection tests. The desktop IPC test
`keyboard_macro_recorder_commands_are_main_editor_only` verifies the main-window capability
boundary. Regenerate DTOs and schemas with `cargo run -p xtask -- types`, then check them with
`cargo run -p xtask -- types --check`. No test in this set should capture or inject real keys.

Real native qualification requires explicit authorization and a disposable scratch target,
never the developer's ordinary desktop or configuration. Record OS/build, architecture,
layout, exact package/build identity, permissions, target identity and achieved capabilities for
macOS ARM64, macOS Intel, Windows x64, X11 and supported Wayland portal sessions. Verify
permission denial/revocation, secure/locked desktop, physical interference, repeat fidelity,
emergency stop, cancellation at every event, device/dial bursts and shutdown cleanup.
Windows builds are intentionally unsigned (`--no-sign`); a publisher signature is not a
macro qualification prerequisite. macOS desktop bundles have an ad hoc integrity seal,
not a trusted publisher signature or notarization. Check Input Monitoring/Accessibility
against that exact app bundle rather than assuming permissions transfer between builds.
Missing platform prerequisites must leave native readiness disabled. Do not substitute
compiled adapters or simulated/browser checks for native or hardware qualification.

<details>
<summary>Native keyboard ownership and platform qualification requirements</summary>

### Scratch-target ownership and teardown

Scratch-target tests have two explicit phases: select a disposable window after the first
countdown, then confirm its displayed identity before the playback countdown. Test always
uses strict target pinning, even when the saved action permits focus changes. The pure
`keyboard_macro::test_session` tests cover both countdowns, target loss, heartbeat expiry,
owner isolation, pending cancellation and teardown failures. Native cancellation is
asynchronous; editor retirement stays blocked until its worker completes cleanup.
Quit/restart separately waits for that worker with a bounded asynchronous drain and
rejects stale drain tickets. A timeout keeps Manager open and reports the blocker.
Recorder Stop/Cancel also wait asynchronously with a bounded response. The session keeps
its keyboard admission lease until native startup and teardown have actually retired,
even if the IPC caller disappears. Cancel can supersede a pending Stop; failed capture
and uncertain teardown are distinct outcomes. Run the recorder's stalled-worker and
startup-cancellation tests when changing these ownership paths. No native hook is used
by those tests.

### macOS safeguards and limits

macOS sleep/session/protected-data observations are defense in depth, not an authoritative
unlocked-state snapshot. The product explicitly accepts best-effort lock detection: the
local probe observed protected-data status changing about ten seconds after screen lock.
macOS admission now uses live prerequisites rather than an unconditional qualification
switch. Keep the editor warning visible when available, and distinguish this accepted
tradeoff from native delivery/permission/device qualification. Unknown or protected facts,
permission loss and observer failures still fail closed. Stop macros before locking.
macOS target lookup uses the foreground `NSRunningApplication` identity and bounded
application/window AX elements. Do not set an AX timeout on the system-wide element:
Apple applies that timeout to the entire Manager process. Keep operation names and AX
status codes in target failures so unsupported notifications, missing focused windows,
permission failures and unresponsive applications are distinguishable without logging
window titles or keystrokes.
Decode macOS modifier changes from the queued event's side-specific flags, not a
later live key-state sample. Playback must retain both aggregate and left/right
flags through chords, repeats and releases, including when both sides are held.
The modifier regression tests use queued fake input and pure output descriptions;
they never capture or post keys. Old recordings that already omitted a modifier
must be edited or recorded again, not repaired by guessing the intended character.

### Linux X11 safeguards and limits

The local KDE X11 session provider now supplies live generation leases to the scoped
input observer, playback and scratch target. It requires the KDE early-lock interface,
a concrete logind session whose display matches the local X server, and an observed
lock/unlock cycle after ordinary startup. Launch while positively observed locked can
bootstrap on the subsequent unlock. It never locks/unlocks or inhibits the desktop.
The read-only session watcher may persist to establish this initial state; keyboard
subscriptions remain scoped to explicitly requested recording/playback. Other desktops
do not inherit KDE's contract merely because they expose a ScreenSaver service.
GNOME X11 uses a separate optional [Session Guard companion](packaging/linux/gnome-session-guard/README.md)
for Shell 45–48. Installation/enabling is explicit, never performed by Manager. The
companion observes only session/shield state on Shell's main loop and supplies an
initial protected/unprotected snapshot with a per-enable instance and generation.
Manager pins its owner to the running Shell and combines it with logind, display,
pending lock/sleep vetoes and existing transaction leases. Do not replace this with
ordinary ScreenSaver false values or assume it grants Wayland input capabilities.
Global capture combines XI2 physical-edge provenance with RECORD source-tagged device
events so server repeats are not silently omitted. RECORD does not subscribe to other
clients' requests/replies or delivered application events. Its only recorded request is
the observer's own content-free GetInputFocus drain marker. Review byte order, device
identity, timestamp wrap, bounded queues, mismatch handling and teardown when changing
this path. Do not treat source inspection or compilation as observed native delivery.
X11 scratch identity uses the selected server-owned window/client resource lifetime,
not a claim that a numeric XRes PID proves the original connection-owner generation.
Keep conservative process checks and sanitized labels distinct from target continuity.
Actual destruction/reparenting, focus and client-lifetime behavior still needs native
qualification. The exact X11 replay subset admits matching ordinary momentary SetMods
actions and tracks its own depressed modifiers, but rejects explicit repeats and
repeat-enabled keys. Initial modifier/group state and all latch/lock/group state must
be neutral. Consequently many common letter-key shortcuts require explicit System mode.
XTEST enters the server's shared auto-repeat path; do not disable user/shared keyboard
controls, approximate holds with taps, or call arbitrary repeat replay implemented.
An explicitly selected `repeat_mode: system` now permits ordinary X11 shortcuts and
holds without changing those controls. The scheduler retains all recorded delays and
checks, but Repeat checkpoints do not send another down. The OS supplies its current
repeat delay/rate; output count/timing are not promised to match the recording or scale
with playback speed. The user selected System as the default for newly created Linux
X11 actions. Existing saved choices are never rewritten; the catalog/serde legacy
default and new actions on other platforms remain Exact.
Validate complete mappings, ownership and action semantics in either mode; never use
System as an automatic fallback after Exact preflight fails.

### Hyprland and Wayland safeguards and limits

Generic Wayland portals lack passive physical-key observation and focused target
identity. The optional [Hyprland Keyboard Guard](packaging/linux/hyprland-keyboard-guard/README.md)
implements those capabilities for the exact Hyprland 0.56.2 source/dependency ABI. Its
source-only compositor plugin is built for an explicitly allowlisted installed Manager
executable and manually loaded by the user. Manager's Rust transport pins the companion
to the actual Wayland compositor peer and uses bounded scoped leases. No installation,
plugin loading, input permission changes or fallback to privileged devices occurs.
Only physical edges/holds are recorded; explicit System mode is required, not selected
automatically. Portal output authorization is separate. Upstream
`xdg-desktop-portal-hyprland` currently advertises InputCapture but not RemoteDesktop;
compiling/loading the companion alone therefore does not enable playback on a stock
Hyprland installation. A compatible RemoteDesktop provider is an additional deployment
requirement, not something this companion installs or supplies. Lock/focus/layout events and
bounded checks cover observed protection; polling-only transitions and the separate
portal-output path are not an atomic desktop-state guarantee. Physical output follows
the configured keymap; observed latch/lock/group deviations revoke further playback,
but a custom mapped action may already have occurred and is not restored. Unlike X11,
this backend does not claim per-key XKB action introspection. See the companion README
for exact boundaries. The C++ companion has compiled and linked against the pinned
Arch Linux x86-64 Hyprland 0.56.2-4 SDK, with source/ELF/dependency evidence beside the
artifact. This is build verification, not compositor loading, Ubuntu compatibility or
native input qualification. Other
compositors and generic portal-only sessions remain unsupported.

Linux native input and device qualification remains deferred. Compile-only checks and regression source additions are not evidence of native input or device qualification.

</details>

### Read-only session qualification probes

These probes are optional diagnostics, not part of ordinary setup. Coordinate any manual session changes before running them.

<details>
<summary>Build and run the read-only probes</summary>

Build the macOS probe without starting it:

```sh
cargo build -p stream-dock-manager-runtime --example keyboard_session_probe --features native-session-probe
```

A coordinated manual observation run is:

```sh
cargo run -p stream-dock-manager-runtime --example keyboard_session_probe --features native-session-probe -- --seconds 120
```

The probe only samples supported session booleans and revocation generations. It does
not capture keys, inject input, change permissions or read a Manager workspace. Its
`observations_healthy` value is not proof that the screen is unlocked. Label manual
lock/unlock, already-locked launch, sleep/wake and user-session-switch phases outside the
JSON output; only claim remote-session support if tested separately. A restricted phase
that remains admissible must be recorded as a limitation, never silently called unlocked.
The accepted macOS best-effort delay is not immediate lock protection. Do not automate
locking or sleeping a developer's computer as an ordinary regression test.

This probe cannot qualify keyboard routing, permission revocation during input, repeat
fidelity or device behavior. Those require separately authorized disposable-target tests
after the session design is established. Production readiness has no environment-variable bypass. A permission grant satisfies
only its live access check; it does not establish native qualification or prompt lock protection.

A separate macOS target/focus diagnostic can test strict revocation without sending keys:

```sh
cargo build -p stream-dock-manager-runtime --example keyboard_target_probe --features native-session-probe
cargo run -p stream-dock-manager-runtime --example keyboard_target_probe --features native-session-probe -- --seconds 300
```

It needs existing Accessibility permission for its actual launching identity and never
prompts or changes permissions. After its three-second selection countdown, it pins one
ordinary foreground window and never retargets. Output contains only booleans, timestamps
and a revocation generation, never application/window text or process IDs; `native_ready`
is always false. Coordinate the selected window and manual lock/unlock before running.
A healthy target observation is not itself proof of unlock. Qualify sticky revocation
across rapid transitions, launch while locked, sleep and switching before relying on it.
Do not treat this diagnostic as permission to capture input or test playback.

For a real KDE Plasma 6.5 X11 session, the separate read-only candidate is explicit:

```sh
cargo run -p stream-dock-manager-runtime --features native-session-probe --example keyboard_linux_session_probe -- --profile kde-plasma-6.5-x11 --seconds 120
```

The profile flag selects a test candidate; it does not verify the desktop version or
permit native input. Its output always reports `native_ready=false`. Initial state stays
unknown until a complete observed lock/unlock cycle. Check owner replacement, sleep,
session switching, stale/epoch leases and early lock transitions. Do not use this result
to qualify GNOME, another KDE version, Wayland, or actual keyboard capture/replay.

</details>

## CI and release reference

Ordinary development pushes and pull requests do not run repository-defined Actions jobs. Maintainers use purpose branches for internal checks/builds and explicit manual workflows on `main` for releases. See the checked-in [workflows](.github/workflows/) below. Building an unsigned artifact is separate from publishing or qualifying a release.

<details>
<summary>Branches, runner labels, prerequisites and release gates</summary>

### Runner selection

CI defaults to GitHub-hosted Ubuntu, macOS and Windows runners. To use registered self-hosted runners, set repository variables to JSON label arrays matching each runner:

- `LINUX_RUNNER_LABELS`: for example, `["self-hosted","Linux","X64"]`, used by quality and Linux desktop/CLI builds. Release planning, assembly and publication use GitHub-hosted Ubuntu runners.
- `MACOS_RUNNER_LABELS`: for example, `["self-hosted","macOS","ARM64"]`, used by macOS native checks. Install the native ARM64 runner on Apple Silicon; use the X64 runner and label on Intel Macs.
- `WINDOWS_RUNNER_LABELS`: for example, `["self-hosted","Windows","X64"]`, used by Windows native checks and desktop/CLI builds.

Desktop installer and standalone CLI builds use separate `MACOS_ARM64_RUNNER_LABELS` and `MACOS_X64_RUNNER_LABELS` overrides, with matching native macOS labels. They do not use the architecture-neutral `MACOS_RUNNER_LABELS` CI override. Installer preflight checks the operating system, Python process architecture, runner architecture and Rust host/target, and rejects Rosetta translation. CI requires a native Intel runner for its Intel installer. Local builds can target either architecture on a Mac with `make package-target`. Unset installer overrides retain the matching hosted defaults.

Runtime qualification uses `LINUX_RUNNER_LABELS`, `WINDOWS_RUNNER_LABELS`, `MACOS_ARM64_RUNNER_LABELS` and `MACOS_X64_RUNNER_LABELS` for its corresponding native targets, with the same hosted defaults when unset.

Set `DEFER_MACOS_X64=true` to omit Intel macOS from the default build-only package and runtime qualification matrices while its native runner is unavailable. Explicit package target selections still apply; publication continues to require all four native targets. Deferred Intel coverage must be recorded separately and is not a successful check. Clear the variable when Intel execution is restored.

Purpose-branch pushes and dispatches use these overrides. Ordinary development pushes and pull requests run no repository-defined Actions jobs. Clearing a variable restores that platform's hosted default. A self-hosted result qualifies the runner's actual OS/architecture, not the hosted fallback label displayed in the job name.

### Branches and publication gates

| Purpose branch          | What it runs                                           |
| ----------------------- | ------------------------------------------------------ |
| `test-build`            | Checks                                                 |
| `package-build`         | Unsigned development artifacts                         |
| `main` (manual only)    | **Prepare Release**, then separate **Publish Release** |
| `runtime-qualification` | Native runtime matrix                                  |

Promote internal-check code by pull request into its matching purpose branch; dispatch internal jobs only from that branch. Branch creation is suppressed. Release preparation and publication are manual Actions on `main`, never triggered by a push. Package product/target selectors can prepare desktop, CLI or both and platform subsets without tags or releases. Publication still requires every native desktop installer, native CLI package and portable CLI ZIP variant. See [packaging and release qualification](packaging/README.md).

### Prepare once, publish the same files

On other repositories, manually running **Prepare Release** on `main` is a build-only rehearsal. It builds installers and CLI packages for all four platform/architecture targets, including Intel macOS even when `DEFER_MACOS_X64` is set. The workflow captures the exact dispatch commit, uploads development build artifacts, and does not assemble `manager-publication`, create tags, or publish a release. Only the canonical product repository can prepare publication artifacts or run **Publish Release** successfully. Forks may also run this read-only rehearsal using their own runners and settings.

1. Review the version and source on `main`. Align `VERSION`, Cargo/npm manifests and locks before preparation. Do not change a version merely to retry publication.
2. Prepare and publish through separate explicit manual workflows on `main`; no readiness environment flag is required. Full vendor, native, installer and physical-device qualification is not required for this initial public release; limitations remain documented.
3. In Actions, run **Prepare Release** (`prepare-release.yml`) on `main`. It takes no inputs and captures that exact commit and checked-in version. It validates pinned compatibility evidence without requiring every row to be verified, then builds desktop installers and CLI native packages/portable ZIPs for Linux x64, Windows x64, macOS ARM64 and macOS Intel. It creates no tag or release. Exhaustive quality and native qualification suites remain separate CI workflows, not preparation dependencies; package structure, integrity and provenance checks remain mandatory.
4. Wait for the **entire run** to succeed. Review its results and record its run ID. The final `manager-publication` artifact contains `catalog.json`, `cli-catalog.json`, `SHA256SUMS`, and the exact release files; it is retained for 30 days, subject to repository retention policy.
5. Separately run **Publish Release** (`publish-release.yml`) on `main`, supplying that ID as `prepare_run_id`. It verifies the GitHub workflow/run/repository/commit/attempt, artifact identity and SHA-256, source version, complete file inventory and checksums. It does not rebuild or execute artifact contents. Only after verification does it create or verify `vVERSION`, upload missing matching draft files, verify the downloaded bytes and make the draft visible.

For an interrupted publication, rerun **Publish Release** with the **same preparation ID**. An identical already-published release is verified without overwrite. A mismatched tag, draft, published file, expired/ambiguous artifact, failed preparation, or stale artifact from an earlier attempt fails closed. Prefer a new complete Prepare run rather than rerunning only failed preparation jobs and mixing attempts. If artifacts expire before publication, prepare again; do not replace an existing published version with different bytes.

Private repositories may be published explicitly without changing their visibility. Their release downloads require GitHub authentication. Successful publication does **not** establish private-release support in Manager's updater or anonymous download availability. Never put tokens in catalog URLs or release assets.

### Provisioning and reproducibility

Provision Git on each runner. Linux quality and desktop/CLI installer jobs require Docker and use a digest-pinned official Ubuntu 24.04 amd64 container. They bootstrap Git and Python 3.12 before checkout, install build dependencies inside the container, and verify Ubuntu userspace, glibc 2.39 and native architecture. Other Linux jobs retain their host prerequisites. Self-hosted macOS requires Xcode Command Line Tools and Python 3.12 or newer on `PATH`; CI creates an isolated Python environment in the runner's temporary directory. Windows requires Visual Studio C++ Build Tools and a Windows SDK with x64 libraries. The job reports missing native prerequisites before compiling. CI installs Rust and Node.js on all platforms.

For a separately provisioned Linux runner, manually dispatch **checks** from `test-build` with `targets=linux-host` to run quality checks directly on that host without Docker or APT changes. The job records its actual distribution, kernel, glibc and source revision, retains the syntax, formatting, static analysis and compilation gates. This result describes that host; the Ubuntu 24.04 release build baseline remains independently checked. Ordinary `test-build` promotions retain their normal container/default-platform coverage.

Official Plugins native validation lives in the Plugins repository's `native-validation.yml`. Its protected `test-build` promotions run Linux x64, Windows x64 and macOS ARM64 checks using that repository's own read-only token and registered runners. This avoids cross-repository checkout permissions. Each result records its actual platform and source; it does not qualify a desktop session or hardware.

Linux container jobs require a clean checkout `target/` and keep Cargo, Rustup, npm and Python state there, preventing reuse of binaries linked on a newer host. Cleanup restores the workspace's captured UID/GID without following symlinks. Other jobs use a dedicated npm cache in the runner's temporary directory so cache restore/upload does not include packages from the runner user's other projects.

Publication uses GitHub CLI (`gh`) supplied by the GitHub-hosted Ubuntu runner; the workflow checks its presence and reports its version. Build-only desktop and CLI jobs do not require `gh`. The standalone CLI workflow uses Cargo without Node.js/frontend compilation and produces native packages plus portable ZIPs separately from the desktop app. Linux CLI packaging installs `dpkg-dev` and `rpm`; Windows CLI packaging verifies and extracts a pinned portable NSIS archive into its owned tools directory. Windows tools are not executed by macOS/Linux jobs. Installer and release jobs use isolated Python environments on self-hosted runners. Release bundles keep downloaded Tauri tools, including NSIS, under the checkout's `target/.tauri` directory. Windows builds need network access for the verified tool downloads; NSIS does not need a separate global installation. Linux installer evidence includes the exact executable digest and GLIBC requirements, rejecting versions above 2.39. A container build does not qualify a graphical session, device access, installation or the host distribution; test those on the declared release targets.

</details>

[Back to Manager](README.md) · [Packaging guide](packaging/README.md)
