# Plugin acceptance fixtures

Use these fixtures to test Manager's plugin host in a disposable configuration. For everyday installation, see the [Manager guide](../../../README.md#add-plugins-and-nodejs).

## Choose a starting point

| Goal                                               | Guide                                                                    |
| -------------------------------------------------- | ------------------------------------------------------------------------ |
| Install plugins for everyday use                   | [Manager plugin setup](../../../README.md#add-plugins-and-nodejs)        |
| Exercise the local test plugin                     | [Counter check](#run-the-local-counter-check)                            |
| Test official Elgato SDK transport                 | [Elgato probe](elgato-sdk-executable/README.md)                          |
| Test official OpenAction SDK transport             | [OpenAction probe](openaction-sdk-executable/README.md)                  |
| Review supported contracts                         | [Compatibility matrix](compatibility-matrix.json)                        |
| Review source and qualification details            | [Vendor contract reference](#current-vendor-contract-work)               |
| Test archive validation without launching a plugin | [Archive fixtures](../../../crates/plugin-host/tests/fixtures/README.md) |

The probes run executable test code. Provision only the pinned dependencies and use the disposable setup described by each guide.

## Run the local Counter check

Install the [Manager build prerequisites](../../../CONTRIBUTING.md) and locate a Node.js 24 executable. Run from the **Manager repository root**, replacing the example Node path:

```sh
python3 scripts/prepare_patched_deps.py
STREAM_DOCK_MANAGER_NODE=/absolute/path/to/node24 \
  cargo test -p stream-dock-manager-plugin-host --test node_protocol \
  node_package_registers_receives_input_and_returns_scoped_feedback -- --ignored --exact
```

This opt-in native integration test requires local loopback sockets. It packages the checked-in Counter source into a disposable host, checks input and feedback, then shuts down and removes its temporary files. The exact test filter avoids selecting the separately built Elgato SDK probe.

## Fixture types

| Fixture                      | Purpose                                                                                                |
| ---------------------------- | ------------------------------------------------------------------------------------------------------ |
| [counter/](counter/)         | Project-owned Node.js plugin for installation, enablement, registration, input, feedback, and shutdown |
| [sources.json](sources.json) | Pinned public sample sources and their license files                                                   |

The local Counter is GPL-3.0-or-later and is not the Elgato Counter sample. It does not establish compatibility with an external SDK.

## Test downloaded samples

Obtain a sample package at the revision recorded in `sources.json` and retain its license. From the Manager repository root, run the acceptance host with that package:

```sh
cargo run -p stream-dock-manager-plugin-host --example acceptance_host -- /absolute/path/to/sample.zip
```

For Node.js samples, also set `STREAM_DOCK_MANAGER_NODE` to an absolute executable path matching the sample manifest. The host selects that runtime explicitly; it does not silently override the sample's version requirement.

The host uses temporary configuration and emits JSON status and effects. It accepts JSON commands on standard input; send `{"op":"quit"}` to shut it down. See [acceptance_host.rs](../../../crates/plugin-host/examples/acceptance_host.rs) for supported commands.

No proprietary plugin binaries are included here. Sample acceptance does not establish physical device or full VSD Craft compatibility.

## Current vendor contract work

Use the [compatibility matrix](compatibility-matrix.json) to see what is verified. The [pinned source inventory](vendor-sources.json) lists each upstream revision, license and hash. Open the reference sections below for exact test boundaries and historical runs; artifact paths may no longer exist.

Jump to [profile evidence](#profile-qualification-evidence-still-required), [manifest validation](#source-backed-elgato-manifest-validation), [key logic](#manager-key-logic), [isolated inspectors](#isolated-vendor-inspector-windows) or the [historical 7.1 run](#direct-production-71-subset-evidence).

`vendor-sources.json` pins the source revisions, versions, licenses and SHA-256 of
all checked-in upstream files. `vendor/vsd/` contains the untouched official VSD
JavaScript sample, including its original package name and different action UUID
namespace. `vendor/elgato/` contains untouched current core protocol types and the
Counter manifest/package metadata. It does not contain a built Counter executable.
None of these files should be rewritten to satisfy Manager-specific metadata.

Run the independent admission/security regressions without launching vendor code:

```sh
cargo test -p stream-dock-manager-plugin-host --lib vendor_contract
```

`compatibility-matrix.json` records verified, partial, missing and unverified work.
A passing unit suite is **not** full vendor compatibility. Every required row must
be verified before making that claim; reference-host, real browser and physical
controller tests remain separate from mock/unit evidence. The production 7.1 subset satisfies the current SDK version gates but does not
establish untouched Counter execution or support for every vendor feature. Manager's product version remains independent of vendor contract versions.

Vendor package identity is taken from the `.sdPlugin` package name (or validated
installed directory), with an explicit manifest UUID checked against package names.
Vendor actions are owned explicitly; packages cannot overwrite another installed
package's action UUID. Vendor packages need no Manager capability descriptor.
A claimed Manager descriptor remains strictly validated. Standard settings,
inspectors and commands do not grant access to Manager storage, desktop input,
telemetry or completion extensions. Standard vendor actions use successful dispatch,
not a fabricated action-completion receipt.

SDK compatibility does not include Marketplace access, DRM, licensing bypasses,
proprietary services, or platform-specific binaries on unsupported systems. Review
both pinned upstream repositories and their package/schema changes before dependency
updates, refresh this inventory and matrix, and rerun conformance before increasing
advertised support. Do not automatically run newly downloaded vendor plugins.

### OS events and optional links

<details>
<summary>Reference details</summary>

macOS application launch/termination and system wake use NSWorkspace notifications.
Windows application transitions use one-second executable snapshots and native power
callbacks; short-lived processes can be missed. Linux uses executable-name snapshots
and logind resume notifications where available; native behavior remains unqualified.
Device-information changes and authored title changes have separate real runtime hooks.
These sources have not been qualified against the reference vendor applications.

Handling `streamdeck://` links is off by default. The packaged macOS app exposes a
separate opt-in requiring an application warning and macOS default-handler approval.
It records the prior application before the OS change, verifies the result, and restores
that application on disable only if Manager is still the selected handler. Startup never
re-registers the scheme. If saved preferences and OS ownership differ, the UI reports
that mismatch. No real OS handler was changed during development/testing.
Windows/Linux safe handler consent/restoration and first-handler registration without a
prior macOS application remain unavailable. Links currently reach only enabled, running
plugins, without waking disabled or inactive plugins. Native OS-handler qualification
requires an explicitly approved test; pure planning tests do not establish it.

Bundled profiles support the source-evidenced, single-page V2 keypad subset in the six
current official SDK Lights-Out archives. Installation requires separate profile
confirmation. Accepted profiles, generated Manager IDs and exact package/device
provenance commit atomically. Switching uses that mapping, never an arbitrary
Manager profile with the same display name. Decline writes nothing; replacement,
retired sessions, reconnects and newer navigation cannot reuse stale approval.
Automatic offers and first-switch requests are bounded, deduplicated and validated
before commit. `Readonly=true`, custom action names, unlinked titles, V3, multipage
content, resources and unknown fields remain explicitly unsupported.

The final macOS aggregate passed 34 profile runtime tests: 7 authentic-archive parser,
7 confined-reader and 20 lifecycle tests, including queued stale navigation/reconnect,
session retirement, external replacement and review-cache recovery beyond the
100-owner automatic scan cap. The 3 host profile tests, 10 manifest tests, 57 related
frontend tests and 2 packaged-preview confirmation checks also passed.

Both genuine SDK 3.0.4 profile integration cases passed in
`artifacts/current-sdk-candidate/snapshot-x8tichds/`: review/decline/accept and full
restart durability, page 1 rejection, page 0/omitted-page switching, prior-page
return, non-vendor navigation invalidation, and actual queued SDK requests rejected
after navigation or reconnect. The candidate passed 7 host transport tests and 3
Runtime integrations; source SHA-256 is
`d15b30a2ff564a78bbec9e5a9a282821d86a463fa8b918278f8d68587ff81eab`.
That historical run used an isolated 7.1 candidate before the production subset
was promoted. Its independently authored Node 24
driver uses the genuine SDK and untouched authentic archive through the real
queue, consent and transaction paths. This does not qualify the reference host,
physical hardware, Windows native execution or the upstream sample executable.

Unix reads use pinned no-follow directory descriptors. The Windows local-disk reader
uses pinned native HarddiskVolume handles and rejects UNC/network/SUBST roots. Its
source review, GNU test linking and Clippy, and MSVC all-target source check passed;
no native Windows reader tests were executed. No official sample executable was run,
and its SDK 3 / minimum 7.1 manifest remains unchanged. Production now declares the explicitly partial 7.1 subset.

</details>

### Profile qualification evidence still required

<details>
<summary>Reference details</summary>

The [official profile guide](https://docs.elgato.com/streamdeck/sdk/guides/profiles/)
specifies manifest-relative archive paths and installation prompts, including deferred
prompts when `AutoInstall` is false. The pinned `core-src/command.ts` defines zero-based
page requests, remembered pages when omitted, and returning to the previous profile
when the profile name is omitted. These are separate behaviors to verify.

The [official 7.1 announcement](https://github.com/orgs/elgatosf/discussions/186)
introduces profile format V3 for embedded resources; V2 exports from 6.0–7.0.3 are
migrated and V3 is not backward compatible. The community deckout V1/V2 description
is therefore not sufficient evidence for a current importer. The current pinned SDK
ships six genuine V2 archives, now retained unmodified in
`vendor/elgato/lights-out-profiles/` with source hashes and MIT license. Their twelve
Current/Default UUID-directory pairs establish the bounded V2 codec used here, not a
general archive specification. No V3 or multipage export is qualified.

To extend the implemented single-page keypad subset and qualify reference-host behavior,
capture an approved disposable reference-host fixture:

1. Record the exact application/SDK versions and platform. Use an isolated test
   configuration, never an existing user's profiles. Create two visibly distinct pages
   containing a harmless test-plugin action, different settings, and one embedded file.
2. Export the profile using the current official application. Retain the original bytes,
   SHA-256, license/provenance, page order, initially selected page, and expected resource
   content. Inspect ZIP entries without executing plugin code or extracting unsafe paths.
3. Record plugin installation with `AutoInstall` true and false, accepting and declining
   the profile-specific prompt. Confirm whether declining leaves any imported state.
4. Record switch requests for page 0, page 1, omitted page after leaving page 1, and
   omitted profile after switching. Include out-of-range pages and unknown names.
5. Repeat with `Readonly` and `DontAutoSwitchWhenInstalled`, a mismatched device type,
   application restart, package replacement, and a same-named user profile. Preserve
   event ordering and action/resource data, not just screenshots of page labels.
6. Only then map archive identifiers to pages. Reject unsupported content explicitly;
   do not sort UUIDs, substitute arbitrary user profiles, discard resources, or execute
   imported actions during installation. Bound compressed/uncompressed sizes and member
   count, and reject duplicate entries, traversal, symlinks and external resource paths.

No reference application or installed plugin was launched to obtain these missing
observations during this task. The bounded V2 implementation is tested; general
profile compatibility and reference-host parity remain unqualified.

</details>

### VSD documentation and reusable SDK inventory

<details>
<summary>Reference details</summary>

The VSD baseline now includes byte-identical MIT C++ plugin SDK definitions,
connection dispatch and action/plugin interfaces at the recorded repository
revision. Their upstream copyright headers and the VSD source license remain
intact; these sources are review evidence and are never built or executed.
`vendor/vsd/documentation-facts.json` records factual command/event/member names
and required/optional annotations from the official SDK documentation. Each page
has its reviewed URL, date and raw response SHA-256. Vendor prose and executable
website scripts are not redistributed. These are mutable-documentation snapshots,
not a versioned complete schema or reference-host qualification.

Run `python3 scripts/vsd_contract_inventory.py --check` to detect missing/stale
VSD command, event and nested manifest-member rows. Refresh documentation only
through explicit source review; the extractor accepts local reviewed HTML and
never fetches or executes upstream code. Updating a snapshot also requires updating
its entry in `vendor-sources.json`. Documentation and reusable core declarations
are not identical: the pinned core also declares `switchToProfile`, `setFeedback`
and `touchTap`. Differences and payload/version semantics remain unqualified.
The generated inventory preserves separate manually reviewed status/evidence fields.

The SDK repository's weekly vendor review monitors official source revisions,
`@elgato/streamdeck`, `@elgato/schemas` and Elgato's official application
release-note JSON feed. VSD Craft has no established authoritative machine-readable
application-version feed; its official download page requires manual review before
release. Source/schema/app version observations do not change Manager's embedded
compatibility contract or establish reference-host support.

</details>

### Registration language and display context

<details>
<summary>Reference details</summary>

`application.language` describes Manager's actual UI language, not the operating
system locale or the inspector browser's preference. It is derived from the
shipped editor's HTML language declaration; the other first-party entrypoints
are checked for consistency. Manager currently renders an English-only UI.
Registration metadata is held per host instance and is never persisted, accepted
from plugin messages, or placed in the static compatibility contract.

Native desktop startup uses the actual primary-monitor scale until an editor
window exists. Main-window creation, movement and scale changes supply that
window's real scale; destroying the editor returns future registrations to the
current primary display. A headless native process cannot be launched with an
unknown ratio: registration fails before creating process/run state rather than
pretending the display is 1x. Disposable protocol fixtures explicitly provide
synthetic metadata and do not qualify a physical display.

Browser plugins and property inspectors resolve `window.devicePixelRatio` in
that connection's own rendering context immediately before their vendor connect
callback. A remote/browser inspector therefore does not inherit a native monitor's
ratio, cannot rewrite the native host context, and cannot replace the host UI
language with its own locale preference. Unknown/invalid ratio or host language
prevents registration instead of emitting a malformed required field. This does
not describe a Stream Dock device's hardware DPI. Mixed-monitor native behavior
and reference-vendor display semantics still require native qualification.

</details>

### Manifest GIF playback

<details>
<summary>Reference details</summary>

Manifest state GIFs use transient content-addressed clips and the existing native
media clock, memory budget, output scheduler, adaptive rate, and delivery tracking.
Hardware and software targets retain separate source/context clocks. Page/source
retirement, sleep/reconnect and idle software previews release playback ownership.
No generated asset is written to the user's media library.

Cold vendor GIF preparation accepts at most 16 MiB of input, 2048×2048 source
frames, 512 frames, 30 seconds of timeline, and 64 MiB of encoded frames; it resizes
to at most 512 pixels and checks a 500 ms decode deadline between frames. It does
not wait behind busy preparation slots. Failures are reported and retained until
a later source update; repeated failing updates are rate-limited. These bounds do
not constitute reference-host or physical-display qualification. Standard
`setImage` GIF input is explicitly unsupported; reset the image to its declared
manifest state to use animation.

</details>

### Source-backed Elgato manifest validation

<details>
<summary>Reference details</summary>

`vendor/elgato/manifest-schema/manifest.json` is the byte-identical Draft 7 schema
exported by the published `@elgato/schemas` 0.6.0 package. The registry tarball,
integrity, extracted-file hashes, license and review date are pinned in
`vendor-sources.json`. Reproduce extraction with `npm pack @elgato/schemas@0.6.0
--ignore-scripts` in a disposable directory; do not reformat the extracted files.
The original package README and MIT notice are retained alongside the schema.

Discovery/package reads and preflight compile that fixed schema once, offline,
and enforce its version-specific shape. Plugin-provided `$schema` values never
load a remote schema. A validated Manager executable descriptor selects the
separate owned-package contract; a bare metadata or dialect marker cannot bypass
the vendor schema. VSD keeps its own documented contract. The only vendor-data
adaptation is filling an absent UUID from already-validated package identity in
an in-memory copy; no source manifest is rewritten.

Schema shape includes required fields, nested types, enums, unknown fields and
Software.MinimumVersion gates. It does not assert implementation of every field:
Readonly bundled profiles and more than 64 states remain explicit enablement blockers.
Automatic bundled-profile installation requires separate per-profile confirmation;
unsupported archive contents remain explicit review errors, never partially imported. `DisableCaching` revalidates image bytes on explicit image,
state and feedback refreshes; it does not add filesystem polling. Action-specific
OS declarations filter the available actions while preserving raw package metadata.
Encoder layout, background with an explicit layout, and trigger-description
defaults initialize the encoder surface without replacing later runtime overrides.
MultiActionImage uses separate software/multi-action imagery and does not replace
the hardware target's state image.

Field-specific warnings describe the bounded profile subset, explicitly requested
key-logic availability, and encoder background without an explicit layout. Manager does not invent a missing
default encoder layout. DefaultWindowSize warns because the macOS isolated popup
implementation still needs native/reference-host qualification, while Windows/Linux
popups remain disabled. Newer software minimums still block independently, and
Node profiler requests already fail explicitly.

Vendor catalogue thumbnails are static 72×72 PNGs with at most 256 KiB of input
per image, 32 distinct decodes and 4 MiB of total source data per package. Encoded
thumbnail output is limited to 512 KiB per package and 2 MiB across a returned
vendor catalogue. Optional artwork falls back when a limit is reached. Encoder
background PNG/SVG input is limited to 2 MiB and requires an explicit layout;
it becomes a bounded 400×200 PNG of at most 512 KiB. Existing native renderer
allocation limits remain in force. These are Manager resource bounds, not claims
of complete reference-host presentation parity.

Built-in feedback item names, types, rectangles, ordering and defaults come from
`vendor/elgato/builtin-layout-facts.json`, transcribed from the official Dials
guide and hash-pinned in the source index. Pixel regressions cover declared
`$X1`, `$A0` and `$A1` image regions and runtime image feedback. This is not a
reference-host trace: font metrics, weight, exact indicator shape/styling and
optical parity remain unqualified. The separately pinned published
`vendor/elgato/layout-schema/layout.d.ts` defines left/center/right alignment
and a `gbar` triangle below its fixed bar. Raster tests verify those semantics,
including runtime alignment changes, range clamping and `bar_h` as indicator
height (default 10), rather than treating it as a fill/progress bar.

Custom layout default pixmaps resolve package-local references before becoming
visible, without requiring a later `setFeedback`. Layout JSON is limited to
1 MiB and 64 items; default pixmaps allow at most 32 distinct sources, 2 MiB per
source, 4 MiB of aggregate source input and 4 MiB of resolved output. Traversal
and outside-package symlinks are rejected. The existing native renderer's decode
allocation limits apply. `DisableCaching` refreshes these defaults on explicit
layout, state and feedback updates; otherwise a repeated layout reference reuses
its resolved images. A rejected layout leaves the previous rendered layout intact.

The vendor's custom `filePath` and `imageDimensions` keywords are annotations,
not executable validators (see its retained README). Existing bounded path,
package identity, executable, runtime and reference checks remain separate; full
reference/image-dimension and reference-host qualification is not claimed.

</details>

### Dial actions

<details>
<summary>Reference details</summary>

Manager assigns turn-left, turn-right and press actions independently. Linked turns
use one editor configuration for both directions, with direction-aware defaults;
press remains separate. Each lane can be cleared or contain a multi-action.
Changes remain drafts until Save.

For Elgato actions the selected lane's application canvas uses `Encoder.Icon`,
independently of action-list and hardware images. Manager never infers one lane's
icon as the whole physical dial, nor applies these metadata semantics to VSD.
Vendor encoder background metadata is accepted by the manifest schema but has
no Manager rendering behavior.

</details>

### Explicit browser resource selections

<details>
<summary>Reference details</summary>

The macOS isolated native inspector wraps the public `WKUIDelegate` file-panel
callback. Only a completed native file selection can create an external-resource
grant. The grant is bound to the exact plugin, inspector token and action context,
expires after 15 minutes, and is revoked with the inspector. It permits at most
32 selected files and 64 MiB per inspector lifetime, with a 16 MiB per-file limit;
directories, final symlinks, non-regular files and changed file identities are rejected.
Selected bytes are copied immediately into private temporary snapshots. Queued
owned writes retain those snapshots until completion, but token retirement still
rejects their commit. No path-taking IPC, HTTP, or vendor WebSocket upload command
is introduced, and plugin HTML never receives Manager's main IPC.

The path projection uses the exact behavior of Elgato-recommended
`sdpi-components` 4.0.1 at `bfd17677540da403e28ecc442110df77be4306ca`. Its file
component reads `input.value`; the sanitizer strips `C:\fakepath\` and URI-decodes
the full selected path. The untouched component, sanitizer and upstream test cases
are retained in `vendor/elgato/sdpi-components/` with their MIT license and source
hashes. Native code projects only its selected path into the clicked input in the
initiating frame before WebKit dispatches its ordinary change event.

Six Rust grant tests cover selected snapshot replacement, exact scope, expiry,
quota, revocation, retained pending bytes and invalid files. Four Node tests use
the genuine pinned sanitizer and cover control targeting, resets and rejection.
A local isolated Chromium input/change probe with an in-memory File confirms the
encoded full path projection. These checks do not qualify an actual macOS file
panel, vendor reference host, or every input widget.
Directory selections, `showPicker()` without the observed file-input click, and
`about:blank` popup file selection currently fail closed.

The pinned VSD sample declares `onFilePickerReturn(url)` and parses the argument
as JSON, but does not define the selected-file payload shape. This implementation
does not invent or invoke that callback; VSD-specific picker behavior remains
unqualified. Existing browser resource access without native selection remains
limited to package files and already-owned action resources.

</details>

## Manager key logic

<details>
<summary>Reference details</summary>

The existing strict `Gesture` action is available as **Key logic (Manager)**. Its
three branches and configurable timing fields are edited in the ordinary action
editor; no profile migration is added. Two releases within the double-press
window form one pair. A third release starts another pair. Hold runs once at its
threshold and waits for release. These are Manager semantics, not measured
Elgato timing. A long second press is a separate hold, not a completed second tap.

The independent `SupportedInKeyLogicActions` flag is projected from installed
metadata and checked in the picker, workspace validation and actual plugin
dispatch, including saved macros and dynamic binding targets. Manager retains its
existing nested multi-action pulse protocol, so Elgato descendants also require
`SupportedInMultiActions`; two-state actions retain desired-state selection.
Explicit key-logic flags need a schema-valid 7.0 manifest, which remains subject to
the unchanged production host-minimum gate. Omitted flags retain upstream true
defaults. See `vendor/elgato/key-logic-facts.json` for source boundaries.

Recognition is scoped by device, binding, branch, connection and cancellation
generation. Retirement fences already queued requests as well as live timers.
Paired taps, cancellation, page/profile changes, edits, disabling, device removal,
held release and shutdown are exercised with disposable configuration roots.
Shared macro graphs have memoized, depth-aware traversal and a 10,000-node work
bound in native admission and the editor. Synthetic tests do not qualify exact
Stream Deck Key Logic wire semantics or physical timing.

</details>

### Isolated vendor inspector windows

<details>
<summary>Reference details</summary>

On macOS, vendor inspectors run in raw Wry views hosted by native windows, not
Tauri webviews. No Tauri IPC handler, initialization or application protocol is
installed. Each inspector has a distinct loopback HTTP origin and nonpersistent
WK data store. Its file grant and browser WebSocket registration are bound to
that origin. Revocation rejects new HTTP requests, WebSocket registration and
message batches; bytes already admitted may finish. Origin tombstones remain
until token retirement, so closing a native tree cannot reopen its grants through
the shared listener. Disable, session retirement and editor closure retire views.
Managed inspector Apply/Cancel continues to use its existing sandboxed iframe.

Every popup requires a native decision naming the plugin and package-relative
destination. No token, query or fragment appears in that prompt. A refused popup
blocks further requests for that inspector session. Native policy allows at most
four live windows, eight creations per session and two descendant levels. Closing
an opener closes its descendants. The session and opener are rechecked after the
modal decision. Navigation remains within the session package, with inherited
`about:blank` children permitted; external, app, IPC, file, data and blob top-level
URLs are denied. Downloads are denied. Media permission callbacks are denied,
fullscreen is disabled and restrictive HTTP Permissions-Policy headers are sent;
this is not a claim that every WebKit permission API is qualified.

Elgato `DefaultWindowSize` is applied to popup children, with the published
500×650 default when absent. Explicit requested sizes override it. Both paths
are finite and bounded to 320–1200 by 240–1000 logical pixels. The root inspector
has its own 620×720 size. VSD does not inherit Elgato manifest semantics.
Linux uses an isolated raw Wry root window with no Tauri initialization,
app protocol, editor opener or Tauri command handler. Popups remain denied.
The only custom IPC request is a bounded UUID and multiple-selection boolean that
opens a native file chooser, never a path or filesystem operation. Navigation
stays in the exact revocable inspector package origin. Files are granted only
from a completed native chooser through the same snapshot/quota checks as macOS.
Completion is correlated to the original main-frame clicked input and pending
native request; cancellation, retired sessions and detached inputs discard it.
The projection supports the pinned sdpi-components full-path contract, not native
FileList/FileReader semantics or unverified VSD callbacks. The Linux native file-chooser callback cancels unhandled picker paths (including
showPicker and directories). Windows retains its sandboxed iframe: the pinned
WebView2 bindings lack a stable native chooser interception API, so the same
fail-closed boundary has not been implemented there. The Linux bridge
still needs real native-panel and vendor reference-host qualification;
DOM/unit tests and macOS type checks do not establish that qualification.

Loopback tests exercise cross-session and shared-listener rejection, Host and
WebSocket Origin checks, a revoked incomplete HTTP request and passive WebSocket
shutdown. Pure native policy tests cover navigation, bounds, re-entrant consent,
revocation during consent and descendant closure. These tests do not qualify
real macOS activation/consent, clipboard/geolocation behavior or a vendor host.
The opt-in native probe runs with:

```sh
cargo run -p stream-dock-manager --example vendor_inspector_probe --features native-inspector-probe --offline
```

It uses only temporary package/config roots and dummy windows, never the installed
Manager, application Runtime, hardware discovery, user files or OS permission
requests. The actual macOS WK run passed root/child IPC and opener isolation,
named-window reuse and opener postMessage, nonpersistent shared-descendant stores,
cross-session Path=/ cookie isolation, navigation denial, default/explicit size
bounds, window/depth limits, subtree closure, revocation and clean exit. Source
hashes and the successful run are in `artifacts/vendor-inspector-probe/result.json`.
Its injected confirmation and automatic script requests apply only to the exact
dummy fixture. They are not real trusted-consent or user-activation qualification;
native file panels, clipboard/geolocation and vendor reference-host behavior remain
unqualified. The optional feature alone does not enable the probe or expose any
IPC command. Normal production popup consent and activation behavior is unchanged.

</details>

## Direct production 7.1 subset evidence

<details>
<summary>Reference details</summary>

The checked-in production declaration and implementation registry were then
qualified directly with unmodified SDK 3.0.4: **7 host tests and 3 real Runtime
tests passed**, including both bundled-profile cases. No copied declaration or
registration version was injected. Evidence is retained in
`artifacts/current-sdk-candidate/production-8vbm656f/`; source SHA-256:
`22607b429d015260935d90dd19a230f846b8f3d9c004eed74cf13b6bce0832c8`.
Source inputs, contract/registry and authentic profile bytes stayed unchanged
through that run. All five logged profile driver processes were confirmed exited.

The separate older-manifest probe also passed: SDK default mode rejects its 6.9
manifest minimum even on host 7.1, while explicit legacy mode works with the
acknowledged Node 20-to-24 override. Its assertion was corrected after the ten-test
run; production code and those ten test sources did not change. Evidence prose
was updated afterward. The full-compatibility gate remains false; the tested application
release was 0.1.0, with reference-host, physical hardware, general profile
formats and native interaction/platform qualification still separate.

</details>

[Manager setup](../../../README.md) · [Contributing](../../../CONTRIBUTING.md)
