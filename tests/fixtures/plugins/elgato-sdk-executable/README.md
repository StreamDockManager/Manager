# Elgato SDK test fixture

This project-owned probe imports the genuine `@elgato/streamdeck` 3.0.4 npm release.
It is not the upstream Counter sample and does not modify or reimplement the SDK.
`package-lock.json` pins the complete dependency graph and registry integrity hashes.
`ELGATO-LICENSE` retains the SDK's MIT notice. The corresponding reviewed upstream
source revision is recorded in the [source inventory](../vendor-sources.json); npm does not publish a gitHead
for this release, so the registry integrity hash, not an asserted commit equivalence,
identifies the executable dependency. Rolldown is also used by the upstream Counter.

## Start here

This is a developer test fixture, not an end-user plugin. `STREAM_DOCK_MANAGER_NODE` selects its Node.js 24 runtime independently of Manager's frontend build toolchain.

| Requirement                           | Purpose                                   |
| ------------------------------------- | ----------------------------------------- |
| Node.js 24 at an absolute path        | Run the pinned SDK probe                  |
| npm and the checked-in lockfile       | Install and build the fixture             |
| Rust/Cargo and Manager's dependencies | Run the disposable host                   |
| Python 3.11+                          | Run the current-mode qualification script |
| Local loopback networking             | Connect the fixture to that host          |

Choose the older-manifest probe below or [current-mode qualification](#current-mode-production-qualification) for the production contract. Both need the pinned npm dependencies. A successful probe is not full Stream Deck compatibility.

### Install dependencies

Run all commands in this guide from the **Manager repository root**, after setting up the [contributor prerequisites](../../../../CONTRIBUTING.md).

```sh
npm --prefix tests/fixtures/plugins/elgato-sdk-executable ci --ignore-scripts --no-audit --no-fund
```

### Run the older-manifest probe

Prepare native dependencies on a fresh checkout, build the probe, then use an absolute Node.js 24 executable:

```sh
python3 scripts/prepare_patched_deps.py
npm --prefix tests/fixtures/plugins/elgato-sdk-executable run build
STREAM_DOCK_MANAGER_NODE=/absolute/path/to/node24 cargo test \
  -p stream-dock-manager-plugin-host --test node_protocol \
  official_elgato_sdk_executes_legacy_and_rejects_older_manifest_in_default_mode -- --ignored --exact
```

The test copies the generated bundle and source manifest into a temporary package,
uses a metadata-only runtime service which rejects artifact downloads, installs into
a temporary Manager host, and communicates over loopback. It never launches an
installed Stream Deck application, opens hardware, accesses real profiles, or performs
network commands. The probe writes a connection result only in its temporary package;
settings writes are confined to that disposable host. Build output and node_modules
are ignored, not vendored generated code.

## Qualification boundary

The production host advertises a **7.1 subset**, not full compatibility. The older-manifest probe explicitly exercises a Node 20-to-24 override; current-mode checks use a 7.1 / Node 24 manifest.

<details>
<summary>Manifest, runtime and settings contract details</summary>

The production host advertises the explicitly partial `streamdeck-7.1-subset`.
The original protocol probe confirms that default mode still rejects its older
6.9 manifest minimum, independently of the 7.1 host version. It then exercises
the explicit `useLegacySettingsBehavior` option successfully. Its declared Node 20
to Node 24 runtime override remains separately acknowledged and checked.
The dedicated current-mode tests below use a schema-valid 7.1 / Node 24 manifest
and normal runtime selection, without the legacy option or a runtime override.

A passing protocol run does not qualify the untouched upstream Counter, inspector
browser execution, hardware or reference-host parity. Those remain separately
tracked in the compatibility matrix, whose full release gate remains false.

The probe manifest now matches the published current Elgato schema: its 6.9
minimum requires `Nodejs.Version: "20"`. The disposable executable test first
asserts that selecting Node 24 without acknowledgment fails, then uses the real
runtime-selection API with explicit override acknowledgment and asserts that the
resolution reports an override. This is protocol qualification under a declared
Node 20-to-Node 24 runtime override, not ordinary 6.9/Node 20 package compatibility.
Production LTS validation is unchanged. Its
four-part vendor manifest version 0.1.0.0 does not change the project release.

The source-valid manifest also has required author/description/OS/icons/states.
Tests generate appropriately sized blank PNG artwork only in their disposable
package; the upstream Counter and official SDK sources remain untouched. Debug
options do not inject process arguments during normal execution, and requested
profiler output is explicitly unsupported.

The current-mode gate protects message-ID distinguished getter responses and
settings cache behavior. The tests cover cache invalidation, getter IDs, PI edit
notifications exactly once, no runtime self-write callbacks, context isolation
and real Runtime resource durability separately from host-only projections.

</details>

## Current-mode production qualification

`current-sdk-audit.json` maps every explicit 7.0/7.1 gate in this pinned SDK and the
corresponding manifest changes to implementation, tests and remaining limitations.
The production contract and independent implementation registry both declare 7.1
as a subset. The vendor contract version is independent of the Manager release version.

After installing dependencies above, build the current-mode probe and run the production check:

```sh
npm --prefix tests/fixtures/plugins/elgato-sdk-executable run build:current
python3 scripts/qualify-current-sdk.py --production --run --node /absolute/path/to/node24
```

The runner records selected source, locks, build assets and fixtures under
`artifacts/current-sdk-candidate/production-*`, including SHA-256 hashes. It runs
Cargo **directly against the checked-in production source**, with a separate target
directory and offline locked dependencies. It never changes a production or copied
contract in production mode and never rewrites registration messages. Source and
contract stability are checked after execution. No installed application, hardware,
real profile, runtime download or user configuration is used. Without `--run`, it
only records the audit snapshot. The historical candidate mode remains available
only for a 6.9 source baseline and changes declarations in its isolated copy only.

<details>
<summary>What the host-stage check covers</summary>

The genuine, unmodified SDK runs with its default configuration. The disposable
manifest declares 7.1 and Node 24, and normal runtime selection must succeed
without override acknowledgment. The suite exercises all four gated SDK
operations: default settings behavior, getResources, setResources and changed
device listeners. It also covers settings cache cloning/invalidation, getter
callback suppression, runtime-write silence, exactly-once PI notifications,
context isolation, disappearance/reappearance and deterministic owner delays.

The host-stage PI peer is a real authenticated WebSocket, not a browser. A
deliberately controlled owner commits host projections, so that stage does **not**
establish runtime resource staging/durability. A failed qualification remains failed;
neither the runner nor test results automatically change the production declaration.
Manager key logic has a separately tested configurable tap/double/hold subset;
exact vendor timing and event parity remain unqualified by these SDK transport tests.

</details>

Add `--runtime` to run the separate genuine SDK-to-Runtime durability stage:

```sh
npm --prefix tests/fixtures/plugins/elgato-sdk-executable run build:profiles
python3 scripts/qualify-current-sdk.py --production --run --runtime --node /absolute/path/to/node24
```

<details>
<summary>What the Runtime-stage check covers</summary>

This uses the existing headless test support with a synthetic device and saved
profile in disposable SQLite storage. Offline metadata is seeded before opening
the real Runtime; automatic metadata checks are disabled. The real settings
writer, workspace transactions and managed resource staging process every SDK
request. No fake owner commit or production-constructor override is used. The
test checks immediate settings/resource roundtrips, another action's isolation,
rejected staging with the previous resource preserved, deletion of the test
source file, and a complete Runtime/SDK shutdown and restart before re-reading
settings, globals and exact managed bytes.

This stage also runs the independently authored `bundled-profiles.js`
driver against the real bundled-profile review and transaction APIs. Its synthetic
package declares the exact owning Lights-Out action IDs/names and state counts so
the untouched distributed `sd.streamDeckProfile` can be validated. Neither the
upstream sample program nor its authentic SDK 3 / minimum 7.1 manifest is executed
or rewritten. The synthetic manifest independently declares SDK 3, minimum 7.1,
Node 24 and `AutoInstall: false`.

Every profile request calls the genuine SDK's `profiles.switchToProfile`. The
headless helper pumps authenticated typed effects into the production queue, with
no manufactured effect or fake commit. A test-only RAII hold pauses dequeue to
test navigation/reconnection after actual admission; release runs the real worker.
The driver respects the normal request throttle. Assertions cover lazy review,
decline without workspace changes, acceptance and full restart, installed-profile
switching, omitted/zero page versus rejected page 1, previous-page restoration,
non-vendor navigation invalidation, and stale queued requests. This is the bounded
observed single-page V2 format, not full V3 or arbitrary vendor-profile support.

Results record host and runtime stages separately. Neither stage qualifies actual
inspector browser/popup behavior, complete vendor profile V3, reference-host
parity, physical hardware, untested native platforms or full release readiness.

</details>

## Direct production 7.1 subset evidence

These are historical results for recorded source revisions, not a claim that the current checkout was just tested. Artifact folders may have been removed by local cleanup.

<details>
<summary>Recorded run details and limitations</summary>

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
was updated afterward. The full-compatibility gate remains false; the application
release remains 0.1.0, with reference-host, physical hardware, general profile
formats and native interaction/platform qualification still separate.

</details>

[All plugin fixtures](../README.md) · [Manager setup](../../../../README.md)
