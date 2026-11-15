# Vendor protocol audit

This is a contributor reference, not a device installation guide. For setup and model
selection, start with the [hardware README](README.md). The dated evidence below is
retained so support decisions remain reviewable; a source match is not a hardware test.

| Looking for                                   | Read                                                 |
| --------------------------------------------- | ---------------------------------------------------- |
| Connect a device and assign a control         | [Hardware setup](README.md#start-with-manager)       |
| Compare model capabilities and exact variants | [Model guide](README.md#find-your-model)             |
| Understand transport and output contracts     | [Protocol evidence](protocol-evidence.md)            |
| Contribute an adapter                         | [Development guidance](README.md#develop-an-adapter) |

## Historical review, 2026-10-27

This section preserves the original review and its then-current gates. The current
revision and dispositions below supersede the historical model table; historical
observations are not erased or promoted to physical qualification.

`git ls-remote` resolved vendor `main` to
`7f0d47fe91033b0883edf29b574a6c74ea9bed00` on 2026-10-27. The reference
checkout was read as source and ELF data; no vendor binary was executed or
redistributed. [Source identities](protocol-sources.v1.json) include both SDK
languages, active factory implementations, configuration definitions and transport
identity. [Registrations](vendor-registrations.v1.json) records individual Python
table rows and active C++ registrations. `scripts/vendor_inventory.py` reproduces
that inventory without importing vendor code. New upstream revisions require review.

The subsequent [VSDinside/TreasLin N3 addition](n3-vsd/README.md) records
`5548:1001` separately in `additional-registrations.v1.json`, using pinned
implementation sources and a local exact-interface identity observation. It does
not alter this manufacturer inventory or resolve any source-conflicting variant.

The subsequent TreasLin retail review adds N1 `5548:1002` as a separate model,
HSV293S/VSD293V3 `5548:1003` to 293 V3, and VSDXL `5548:1034` to XL. The
[hashed manufacturer-app identity table](retail-protocol-sources.v1.json) and
[independent N1 sources](n1-vsd/protocol-sources.v1.json) record this evidence.
These additional registrations do not enable the disputed `5548:6670` 293s or
change the original N1 controls. N1's shared CRT mode offset 2 emits **ASCII 3**;
the retail init selects MOD before DIS/LIG and restores brightness afterwards.
Its independent input reader requires ACK framing without the manufacturer
SDK's additional OK marker. These exceptions apply only to the retail N1 codec.

The N1 preview correction separates the two plain inputs from their strip image
regions in both descriptors. `key-16`/`key-17` retain input codes 30/31 and refer
to `display-1`/`display-2` for feedback. Slots 16/17/18 and their existing pixel
sizes remain unchanged; the three adjacent display regions render as one strip.
Saved input bindings retain their appearance through these feedback links.

[The baseline](protocol-audit-baseline.v1.json) records the actual pre-change Manager
selectors, controls, pixel sizes, capabilities, transforms and codec tables for each
model. It is historical evidence, not a source for generating descriptors. The
existing [per-model protocol matrix](protocol-evidence.md#per-model-contracts) provides
the corresponding Python method-derived expected values. Stable IDs are retained.

| Local model | Vendor model/methods                                       | Comparison and disposition                                                                                                                                                                                                                                                                                                                                         | Verification                                                                          |
| ----------- | ---------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------- |
| 293         | `StreamDock293`; C++ `StreamDock293V2` constructor         | 5500:1001; 100² JPEG/180°; 513-byte output; raw row-major background agree. Keep model codec.                                                                                                                                                                                                                                                                      | Synthetic fixture, model tests                                                        |
| 293 V3      | `StreamDock293V3`                                          | Python additionally registers 6603:1010; same callable implementation as 1005/1006. 112² JPEG/180°, 1025-byte output.                                                                                                                                                                                                                                              | Product-table rows and synthetic fixture                                              |
| 293s        | `StreamDock293s`; C++ `StreamDock293sV2`                   | Same 5548:6670 registration; Python 85² main keys versus C++ 96². Physical matching is unavailable pending cross-language geometry reconciliation. The Python-derived simulator reference and persisted profiles remain readable.                                                                                                                                  | Both source hashes; existing transform/input fixture                                  |
| 293s V3     | `StreamDock293sV3`                                         | 6603:1014, 96²/80² JPEG, vendor +270° rotation (= clockwise 90°), 1025-byte output agree.                                                                                                                                                                                                                                                                          | Model fixture/codec conformance                                                       |
| N1          | `StreamDockN1.open`, image and input methods               | 6603:1000/1011; 96²/80² displays; input 30/31 differs from output slots. Keep display 18 independent. Background firmware threshold remains unresolved and unavailable.                                                                                                                                                                                            | Retired-ID/persistence and decoder fixtures                                           |
| N3          | `StreamDockN3`; C++ N3V2/N3V25                             | Python has six registered variants. C++ additionally has 6602:1000, and disagrees on 6602:1002 transforms/background encoding (raw RGB16 versus JPEG) and pulse state. Do not add the C++-only variant or infer another codec. Physical matching for 6602:1002 is unavailable pending variant/firmware evidence; other established variants retain their mappings. | Registration diff, model-owned mapping fixtures                                       |
| N4          | `StreamDockN4`                                             | 6602:1001/6603:1007, 112²/176×112 JPEG, model-specific swipe codes. 5548:1004 is absent in both tables; removed from active hints.                                                                                                                                                                                                                                 | Exact selectors and negative inventory tests                                          |
| N4 Pro      | `StreamDockN4Pro`; C++ `streamdockN4Pro.cpp`               | Python registers 5548:1008/1023/1021; C++ omits 1023. C++ raw main-key map agrees with Manager. Python reverse-map interpretation of 1…15 conflicts; do not invent a fifth touch control. See input and command notes below.                                                                                                                                       | Vendor source, independent raw reports, existing captured observations kept unchanged |
| XL          | `StreamDockXL`                                             | 5548:1028/1031, 32 image keys, six switch edges; 80² PNG/180°. Remove benchmark truncation to ten keys.                                                                                                                                                                                                                                                            | Complete-key workload and model fixtures                                              |
| M18         | `StreamDockM18`; C++ constructor/changeFirmwareVersionMode | Python has 6603:1009/1012. C++ also actively registers 5548:1000, so that candidate is source-backed, but firmware modes change background/lighting support. All M18 physical variants remain unavailable pending exact firmware-mode reconciliation; the candidate is no longer an active discovery hint.                                                         | Active factory and firmware-mode source preserved                                     |
| M3          | `StreamDockM3`                                             | Python additionally registers 5548:1032, using the same implementation. 96² PNG, clockwise 90°; retain registrations. C++ vibration enum alone grants no vibration capability.                                                                                                                                                                                     | Registry and model fixture                                                            |
| K1 Pro      | `K1Pro`                                                    | Three registrations; both SDKs explicitly select report ID 4. The original report-ID disagreement was an audit error: the third setReportSize argument is feature size. The 2026-11-07 K1 review enables experimental matching, follows Python 82efe4d image/input map 5,3,1,6,4,2 and input normalization. Background remains unavailable.                        | ID/chunk boundaries and negative-background test                                      |
| Mini        | `StreamDockMini`                                           | 5548:1036/1037, 64² JPEG/90°, two independent three-position switches and 12 LEDs agree.                                                                                                                                                                                                                                                                           | Per-position edges and transformation fixture                                         |
| H1 Pro      | `StreamDockH1Pro`                                          | 5548:1030/1033, key mode 1, twelve 64² JPEG keys. Whole-panel upload/storage/boot modes remain excluded.                                                                                                                                                                                                                                                           | Mode/slot/input fixture                                                               |

## K1 Pro reconciliation, updated 2026-11-08

The [K1 source inventory](k1-pro/protocol-sources.v1.json) now records the
user-selected Python revision 82efe4d97e6bdbb9b10cbf1a80fd53d8393b278e. Its image
and input addresses are both 5, 3, 1, 6, 4, 2; input state 1 is press and other
states are release, while rotations ignore state. Keyboard color has three zero
bytes before RGB. Effects follow LMOD. Speed now follows the official web app LSPD command with
levels 0–5, superseding Python LSPE 0–7 after reports of unchanged speed. K1 retains the 1023-byte image payload plus sacrificial pad and an
explicit HIDAPI report ID 4; it does not copy Python's omitted report ID or
conditional leading-zero workaround. See [the K1 audit](k1-pro/README.md#protocol-reference)
for all operations, deliberate safety differences and remaining hardware checks.
No physical qualification is claimed.

## Shared wire and scheduling facts

The pinned `TransportSession::transact` (0x19cb0), `chunkSize` (0x16cd0),
`Transport::setKeyBitmap` (0x1af80), and background methods establish report
padding and complete multipart transactions. `GifController._gif_work_loop`
uploads due key streams then calls `refresh`; C++ `gifWorkLoop` likewise batches
refreshes. Grouped key updates are implemented by the model codec with an explicit
STP boundary. Touch, display-only, background and ordinary commands split groups.
This does not claim atomic LCD presentation or a sustainable firmware FPS rating.
Host targets are bounded at 24 FPS with latest-only queues and existing backoff.

Shared discovery follows exact registered VID/PID, usage 1 and usage page >1025.
N4 Pro retains the observed narrow interface 0/page 0xffa0 predicate; it is a
restriction of vendor discovery, not evidence to broaden Linux grants. No new
keyboard/consumer interfaces receive permission. 5548:1002 N1 and 5548:1004 N4
remain absent from both active vendor registration sets. Constant-only N3
`USB_VIDN3V2=0xeeef` and commented M18 factory alternatives are not registrations.

N4 Pro BGPIC disassembly at 0x1c6d0 writes length then **x, y, width, height**
and zero/layer. The previous protocol document incorrectly described width first;
the existing Manager field order is correct. Logical 180° rectangle positioning
remains model-owned. Raw ARX coordinates agree in both SDKs. Vendor code labels
0x38/0x39 as left/right, conflicting with captured same-byte opposite trajectories
on the recorded 5548:1023 unit with unknown firmware. Those captures and the
existing trajectory-based behavior are retained; they do not establish behavior
on the other variants. No evidence or stable IDs have been rewritten.

## Firmware and vibration source gates

The pinned ELF `Transport::getFirmwareVesion` (0x16ea0) calls PLT 0xe560,
relocation 0x2e298 **hid_get_input_report**. It does not use a feature report,
interrupt reader, CRT query or USB release number. The Python wrapper requests
64 bytes and selects the first nonempty NUL-delimited string. N4 Pro detection
uses the existing reader thread, bounded requests, epoch checks and conservative
version recognition. Full model-qualified manufacturer version strings and older
decimal-only responses are preserved. Input/unknown/malformed replies remain unknown.
Native device testing of this operation is still required.

The initial manufacturer-page review found updater instructions but no release
metadata. Follow-up read-only inspection of VSD Craft `3.10.205.0918` on 2026-10-27
identified its public release catalog. Checks
compare only fresh installed versions with the exact release prefix. The
MiraBox 02.006 vibration requirement is a feature minimum, not a latest release.
Its applicability is recorded for the exact MiraBox variant; other branding is
not silently assigned that firmware version. There are no firmware downloads,
bootloader commands or flashing operations.

`Transport::setDeviceConfig` (0x1d1a0) uses the template at 0x24f8d:
`CRT 00 00 QUCMD`, followed by six fields. `DeviceConfig.py` and C++
`N4ProConfigEnumerate` agree on LED following, disconnected lighting, USB power,
vibration, USB reporting reset and boot video, in that order. Both serialize
default and enabled as 0x11, disabled as 0xff. Neither establishes a preserve-current
operation for neighboring settings, nor a configuration readback. Sending five
default bytes would therefore guess about unrelated lighting/power/boot behavior.
Manager now handles that coupling through an explicit first-apply review: the user
chooses every related field before a configuration can be sent. No preserve-current
behavior is inferred, and no unknown field is populated with a default. The model
codec writes the six reviewed values through the existing output worker, with
per-connection deduplication and sleep deferral.
There is no strength slider or simulated tactile effect. Physical qualification
remains outstanding.

## Outstanding qualification

The source disagreements above are actionable gaps, not resolved physical claims.
They require additional evidence from this vendor repository or exact-variant
firmware observations before changing protocol behavior. Disputed physical variants are excluded from native matching and permissions; their
source-derived simulators and saved profiles remain available. Existing codecs and
captures are not reclassified as physical qualification. Native
webview decoder coverage, five-minute CPU/RSS/IPC measurements, optical throughput,
screen-reader qualification and physical firmware/vibration tests must be recorded
separately. Performance acceptance thresholds cannot be claimed from browser
fixtures or a successful build.

## Codec refresh, 2026-11-07

The current source baseline is MiraboxSpace/StreamDock-Device-SDK at
`fc124cb5bc8fe0cd6c72d3f2af80588adc7dcffb`. The source inventory retains the older
`7f0d47f` records and adds Git blob/SHA-256 identities for current model methods,
image helpers, input types, product table, transport wrappers and C++ constructors.
The current registration inventory is reproducible by `scripts/vendor_inventory.py`;
its historical registration snapshot is retained separately in the same JSON file.
No vendor executable was run or copied into the product.

### Revision-pinned model, variant and firmware matrix

All rows below refer to that exact revision unless explicitly marked independent.
Python source paths are `Python-SDK/src/StreamDock/Devices/<class>.py`; the shared
encoding reference is `Python-SDK/src/StreamDock/ImageHelpers/PILHelper.py`.
Dimensions are input canvases; rotations here use Pillow's counterclockwise convention.
Each row is source evidence, not a physical result. Boot columns record upstream format evidence, not permission or enabled Manager capabilities.
Where enabled, boot means explicit persistent LOG upload, never ordinary live background
rendering. Existing N1, H1 Pro and K1 Pro boot capability gates remain unchanged.

| Model / variant or firmware                                    | Image and input facts                                                                | Boot canvas and encoding                                         | Disposition / source                                                                                                              |
| -------------------------------------------------------------- | ------------------------------------------------------------------------------------ | ---------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| 293 / 5500:1001                                                | 15 buttons, 100² JPEG / 180°                                                         | 800×480 BGR888, row-major after 180°                             | Experimental; StreamDock293                                                                                                       |
| 293 V3 / 6603:1005,1006,1010                                   | 15 buttons, 112² JPEG / 180°                                                         | 800×480 JPEG / 180°                                              | Experimental; StreamDock293V3. Retail 5548:1003 remains independently evidenced                                                   |
| 293s / 5548:6670                                               | 15 input buttons; slots 16–18 are display-only, 85²/80² JPEG / +90°                  | 854×480 BGR888 / +90°, row-major after transform                 | Experimental matching enabled: current StreamDock293s and C++293sV2 now agree on 85² geometry                                     |
| 293s V3 / 6603:1014                                            | 15 input buttons; slots 16–18 are display-only, all 96² JPEG / +90°                  | 854×480 JPEG / +90°                                              | Experimental; StreamDock293sV3                                                                                                    |
| N1 / 6603:1000,1011                                            | 15 image keys 96²; plain inputs 30/31; three 80² display-only slots 16–18            | 480×854 JPEG / 0°; 302080-byte soft target, 522240-byte hard cap | Experimental; StreamDockN1. Retail N1 is not this codec                                                                           |
| N3 / vendor 6602:1000,1002,1003,2929                           | Six 60² JPEG image keys / 0°; three plain keys and three encoders                    | 320×240 little-endian RGB565 / −90°                              | Experimental; StreamDockN3 vendor-ID branch; 1000 registration added and 1002 gate lifted                                         |
| N3 / 6603:1002,1003 and 1500:3001                              | Six 64² JPEG image keys / −90°; same input mapping                                   | 320×240 JPEG / −90°                                              | Experimental; StreamDockN3. V25.N3 disables device key-GIF support in upstream; no firmware-storage animation capability inferred |
| N4 / 6602:1001,6603:1007                                       | 10 main 112² and 4 auxiliary 176×112 JPEG / 180°; four encoders; swipes only state 0 | 800×480 JPEG /180°, no horizontal flip                           | Experimental; StreamDockN4. N4 Pro trajectory policy remains separate                                                             |
| N4 Pro / existing variants                                     | Existing model-owned image/input/trajectory behavior retained                        | Existing model-owned policy retained                             | StreamDockN4Pro; recorded captures retain original unit/session limits                                                            |
| M18 / 5548:1000,6603:1009,1012                                 | Current Python registers all three; V2.M18 changes key-GIF and LED flags             | 480×272 JPEG /0° source branch                                   | All variants remain unavailable: firmware-mode reconciliation incomplete; StreamDockM18 and C++M18 variants                       |
| M3 / 5548:1020,1032                                            | 15 image keys 96² /+90°, three encoders; source-selected JPEG/PNG                    | 854×480 JPEG /+90°                                               | Experimental; StreamDockM3; live background separate                                                                              |
| XL / 5548:1028,1031                                            | 32 image keys 80² /180°, two three-position switches; source-selected JPEG/PNG       | 1024×600 JPEG /180°                                              | Experimental; StreamDockXL; independent retail 5548:1034 remains preserved                                                        |
| Mini / 5548:1036,1037                                          | Six 64² JPEG keys /+90°, two three-position switches                                 | 320×240 JPEG /+90°                                               | Experimental; StreamDockMini                                                                                                      |
| K1 Pro / 6603:1015,1019 and 5548:1025                          | Six 64² JPEG keys /−90°; image/input slots 5,3,1,6,4,2; report ID 4                  | 800×480 JPEG /180°                                               | K1Pro boot format is source-backed but not enabled in Manager; existing boot/live-background capability gates retained            |
| H1 Pro / 5548:1030,1033, positively recognized V25/V3 firmware | Twelve 64² JPEG keys /+90°                                                           | 320×240 JPEG /+90°                                               | Experimental firmware-dependent output; StreamDockH1Pro                                                                           |
| H1 Pro / legacy or unknown firmware                            | Upstream non-V25/non-V3 catchall says 70² JPEG /0°                                   | Source catchall: 320×240 RGB565 /−90°                            | Fail closed: no positive legacy firmware discriminator was established; absence of a V25/V3 marker does not qualify legacy output |
| N1-VSD /5548:1002 and N3-VSD /5548:1001                        | Independently pinned retail codecs and input contracts retained                      | Unavailable                                                      | Independent inventories under n1-vsd and n3-vsd; official model changes do not override retail evidence                           |

Current official decoders reject states outside 0/1. Button edges remain independent;
encoder rotations use state 0. N3/N4 encoder presses preserve both button edges while
existing pulse-style actions execute once on press; other pulse models retain their
explicit model policy. N4 swipes also require state 0. Retail input policies are unchanged.
M3/XL use an explicit source-format policy: host-selected PNG remains lossless PNG,
including opaque PNG and images with alpha; JPEG sources remain JPEG. This preserves
the host lossless-output choice rather than silently converting opaque PNG to JPEG.
Only bounded PNG/JPEG inputs are accepted. Upstream instead chooses JPEG for opaque
input and PNG when alpha is needed.
Static JPEG quality starts at 100 and reduces by 2 toward a 9000-byte soft target;
static PNG has a 29000-byte soft target. A soft target is not a hard packet limit.
Upstream has a quantized fallback; Manager deliberately preserves PNG alpha without
that quantization and uses descending JPEG quality without a quantized fallback.
Animated JPEG soft targets are 3000 bytes (XL 2040); animated PNG uses 20000 bytes.
These are host compression policies, not physical display qualification. Enabled boot
JPEG uses quality 100 and a 522240-byte hard cap. The upstream N1 302080-byte soft
target is recorded as source evidence only because its boot capability remains disabled. Raw boot output preserves exact transformed row order and RGB565 little-endian
byte order. Manager's import limits and fit/crop choices remain separate host safeguards.

### Required verification and remaining limits

Exercise each image-slot/input-code distinction, both legal states and rejected states,
raw transformed corners and byte order, boot canvas/caps, transparent/opaque encodings,
N3 vendor branches, H1 unknown/recognized firmware branches, and unavailable M18 variants.
Check display-only surfaces reject SDK action contexts while persisted
button bindings on those same stable IDs remain readable and inactive. Existing live-background scheduling,
N4 Pro trajectories, K1 report-ID framing, retail codecs and reconnect behavior are
regression requirements. Synthetic fixtures and decoded image tests establish source
conformance only. No physical unit, firmware, USB transfer, optical orientation,
restart persistence or performance qualification was available for this refresh.
