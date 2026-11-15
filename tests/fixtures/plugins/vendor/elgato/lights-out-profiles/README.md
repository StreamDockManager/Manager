# Authentic distributed single-page V2 profile fixtures

These six untouched `.streamDeckProfile` ZIP files and the untouched plugin
manifest come from `examples/lights-out/com.elgato.lightsout.sdPlugin/` in
https://github.com/elgatosf/streamdeck/tree/9d9636e9ea956ff4be0246e7ab8e3679866b324f .
The upstream MIT license is included. SHA-256 digests are pinned in the parent
`vendor-sources.json` inventory. No plugin executable is included or executed.
The manifest is SDK 3 / minimum app 7.1; it is not rewritten to bypass admission.

## Using these fixtures

| Task                            | Where to start                                                      |
| ------------------------------- | ------------------------------------------------------------------- |
| Install a plugin for normal use | [Manager setup](../../../../../../README.md#add-plugins-and-nodejs) |
| Run SDK/profile conformance     | [Official SDK probe](../../../elgato-sdk-executable/README.md)      |
| Check provenance                | [`vendor-sources.json`](../../../vendor-sources.json)               |

Do not edit the distributed manifests or archives. The description below records the intentionally supported profile subset and the limits of historical test evidence.

The parser intentionally supports only the observed V2 structure: one active
Keypad page, a distinct empty default page, exact declared device geometry,
empty Encoder/Neo companions only where present, and no image/resource files.
The twelve UUID-to-path pairs establish the observed big-endian UUID bytes to
unpadded base32hex, simultaneous U-to-V / V-to-W substitution, then Z suffix.
ZIP directory records may be present or omitted: tests repack the unchanged
manifest bytes from all six originals with every subset of the six known directory
records. Those derived ZIP containers test transport interoperability, not a new
vendor schema or an authentic directory-less export. Unknown paths, extra files,
missing manifests, duplicate entries, and unsafe archive structures still fail
closed. The original pinned archives are never modified.

These fixtures do not establish general V3 or multi-page ordering. These are distributed
upstream fixtures, not reference-host import/export or physical-device evidence.

Model geometry (columns by rows): sd 5x3 / type 0; sd-mini 3x2 / type 1;
sdxl 8x4 / type 2; sd-mobile 8x8 / type 3; sdp 4x2 / type 7;
sd-neo 4x2 / type 9. ActionID is a source-instance UUID, never a Manager ID;
action UUID must resolve to the exact declaring plugin's Keypad manifest action.

The accepted source action name must equal its declaring manifest action name and
LinkedTitle must be true. All eight per-state typography fields and the selected
state are preserved; custom names, unlinked titles, resources and unknown fields
are rejected rather than dropped. These fixture files are never rewritten for tests.

## Historical verification

The following records describe specific past runs. They do not establish current native-platform or full vendor compatibility; disposable artifact folders may no longer be present.

<details>
<summary>Recorded runs, source hashes and limitations</summary>

The final macOS aggregate passed 7 parser tests covering all six archives, 7 Unix
confined-reader tests and 20 disposable lifecycle tests. Host profile tests (3),
review-related frontend tests (57) and packaged-preview confirmation checks (2)
also passed. The Windows local-disk reader has source review, GNU linked tests and
Clippy, and MSVC source checks only; native Windows execution remains unqualified.
Both genuine SDK 3.0.4 profile cases passed in the isolated 7.1 candidate
`artifacts/current-sdk-candidate/snapshot-x8tichds/` (source SHA-256
`d15b30a2ff564a78bbec9e5a9a282821d86a463fa8b918278f8d68587ff81eab`).
They use an independently authored Node 24 driver with the untouched `sd` archive,
real review/decline/accept/restart/switch/return transactions and actual queued SDK
requests rejected after navigation or reconnect. Candidate host transport passed
7/7 and Runtime integration passed 3/3. The authentic source manifest and archive
hashes were unchanged, and production remained 6.9 during that historical run.
Production now declares the explicitly partial 7.1 subset. Reference-host behavior and the
official sample executable remain unqualified.

## Direct production 7.1 subset evidence

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
