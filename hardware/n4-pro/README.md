# N4 Pro

Use this device with [Stream Dock Manager](../../README.md). Its adapter is included with the app; **no separate adapter download is needed**.

| At a glance                   | Details                                                                           |
| ----------------------------- | --------------------------------------------------------------------------------- |
| Model ID                      | `n4-pro`                                                                          |
| Adapter status                | Experimental                                                                      |
| Registered controls           | 10 buttons, 4 encoders, 4 touch zones and 1 touch strip                           |
| Hardware testing              | Hardware tested on a user unit; full variant/platform qualification remains open. |
| Exact layout and capabilities | [Device descriptor](device.toml)                                                  |

Registered controls describe the Manager model, not a promise that every control or firmware has been physically verified. Dials are called encoders in the descriptor. Match your USB variant below before relying on a retail product name.

[Get started](#get-started) · [Compatibility](#compatibility) · [Troubleshooting](#if-it-does-not-work) · [Contributor checks](#for-contributors)

## Get started

1. [Install or build Manager](../../README.md).
2. Quit VSD Craft and other software that may be using the device.
3. Connect the device, open Manager, and select it.
4. Choose a profile, select a control that supports actions, assign an action, and save the binding.

You can explore the layout in the simulator without a connected device. On Linux, follow the [installation instructions](../../packaging/install-instructions/linux/INSTALL-INSTRUCTIONS.md) and reconnect after installing applicable USB access rules. Run Manager as your normal desktop user.

## Compatibility

| Variant          | USB vendor:product | Physical matching      |
| ---------------- | ------------------ | ---------------------- |
| VSDInside N4 Pro | `5548:1023`        | Enabled experimentally |
| MiraBox N4 Pro   | `5548:1008`        | Enabled experimentally |
| MiraBox N4 Pro E | `5548:1021`        | Enabled experimentally |

All variants remain **experimental**. Enabled matching is not a claim of full firmware, platform, or hardware qualification. Exact interface/usage selectors and capabilities are defined in [device.toml](device.toml).

Some source-derived interfaces intentionally have no automatic Linux permission grant. Do not work around this by making HID devices world-writable.

## If it does not work

| Symptom                                | What to check                                                                                                    |
| -------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| Device is not listed                   | Quit competing device software, reconnect USB, and compare the USB identity with the table above.                |
| Access is denied on Linux              | Check the package's USB rules and reconnect. Some interfaces deliberately remain unqualified.                    |
| A control or image behaves differently | Confirm the exact model/variant and consult the protocol notes below; similar names can use different protocols. |
| Firmware version is unavailable        | See [firmware-read limitations](../README.md#firmware-checks). An unknown version is not treated as current.     |

## For contributors

From the **Manager repository root**, after setting up the [contributor prerequisites](../../CONTRIBUTING.md):

```sh
python3 scripts/prepare_patched_deps.py
cargo test -p stream-dock-manager-hardware-n4-pro
cargo run -p xtask -- hardware check --model n4-pro
```

These checks validate source behavior and fixtures. For physical qualification, follow [verification/plan.toml](verification/plan.toml) on the exact unit, firmware, and platform. Simulated or replayed tests are not physical measurements.

<details>
<summary>Protocol notes and verification details</summary>

Ten LCD keys, four push encoders, four touch zones and a strip, background output and knob lighting. The descriptor defines stable controls, geometry, slots and precise HID interface selectors. Bounded physical observations are recorded below; the complete per-variant qualification procedure remains outstanding.

### Protocol provenance

The authoritative protocol source is the revision-pinned vendor SDK, as recorded in the [current audit](../protocol-audit.md). Community references below remain historical discrepancy evidence:

- [N4 family reference](https://github.com/ambiso/opendeck-akp05)
- [Vendor Python N4 Pro source](https://github.com/MiraboxSpace/StreamDock-Device-SDK/blob/7f0d47fe91033b0883edf29b574a6c74ea9bed00/Python-SDK/src/StreamDock/Devices/StreamDockN4Pro.py)
- [Vendor GIF frame scheduling](https://github.com/MiraboxSpace/StreamDock-Device-SDK/blob/7f0d47fe91033b0883edf29b574a6c74ea9bed00/CPP-SDK/src/DeviceInfo/Feature/GifController/gifcontroller.cpp)
- [Community protocol notes](https://github.com/klaertus/lite-deck/blob/7f0d47fe91033b0883edf29b574a6c74ea9bed00/proto/PROTOCOL.md)

No proprietary transport binary, vendor artwork or firmware is included. The general input fixture is synthetic. The earlier [observed swipe replay](fixtures/swipes.observed-replay.json) reconstructs reports from decoded-coordinate observations and is not a raw HID capture. Its fixed-code hypothesis failed the subsequent physical retest. The [captured swipe fixture](fixtures/swipes.captured.json) retains actual report prefixes, lengths and hashes from a bounded macOS ARM64 capture; all omitted padding bytes were verified zero. It includes the same `0x39` terminal bytes after opposite motions and a trajectory with no terminal. Source/binary identities, timing scope and unknown firmware status remain explicit. Community physical results do not certify this adapter.

### Setup and limitations

Close VSD Craft and other device managers before opening this application. On Linux, install the generated udev rule, reload rules and reconnect the device. Run as the desktop user. The interface must match usage page `0xffa0`, usage `1`, interface `0`; the keyboard interface is excluded.

USB identities `5548:1023`, `5548:1008` and `5548:1021` are candidates, with separate variant records. Firmware identity and USB release number are recorded separately; a USB release number is not a firmware version.

The physical top-row slots are 11–15, bottom-row slots 6–10 and touch-zone slots 1–4. Images rotate 180 degrees. PNG is the initial device format; JPEG encoding is tested in software. Encoder presses are pulses; no encoder hold/push-to-talk is advertised. The supplied unit produced identical `0x39` terminal packets after both left and right motion, so neither vendor code names nor a simple swap reliably identify direction. The decoder now tracks a touch start and the latest valid coordinates, and emits a swipe only when `0x38` or `0x39` explicitly ends the gesture. Increasing x means right and decreasing x means left; the host maps left to next page and right to previous page. Missing, expired or zero-displacement trajectories produce no swipe rather than guessing from the code. A coordinate-only gesture without a terminal also produces no swipe.

The host reserves horizontal swipes for wrapping page navigation and vertical swipes for wrapping profile navigation. Captured vertical completions are `0xB1` for up (next profile) and `0xB2` for down (previous profile), with state zero. Quick flicks can supply only one coordinate sample, so these verified completions supply direction after a fresh start and at least one valid coordinate; clear horizontal or contrary sampled motion is rejected. Horizontal trajectories retain a 24-pixel dead zone and 1.5:1 dominant-axis threshold. Touch-zone completion and gesture terminals consume the trajectory once. The descriptor enables all four directions.

Trajectory state is consumed by a swipe and cleared by a new start, tap end, malformed/unrecognized reports, out-of-range coordinates, sleep, clear or initialization. Coordinates are bounded by the 800 × 480 device plane. The two-second maximum gap between coordinate reports is a conservative safety bound, not measured firmware timing or a maximum total gesture duration. Continuous movements can be longer. Firmware, other release variants, initialization and heartbeat need complete qualification. The all-black lighting quirk uses zero lighting brightness.

On October 24, 2026, a bounded physical retest of immutable Manager `5b8b10d1ff5ed80f4ce56026a56426f37e0f6b61` passed both swipe directions on the supplied `5548:1023` unit under macOS 26.6.2 ARM64. The user explicitly confirmed TOUCH1 (leftmost) → TOUCH4 (rightmost), recorded as x69→657 and `right`, then TOUCH4 → TOUCH1, recorded as x709→117 and `left`. The public-API driver used unchanged production source and an isolated disabled-action profile. It stopped normally after 55.568 seconds with 62 decoded inputs, no observed event gaps and no reported errors. The earlier ambiguous four-gesture order and failed fixed-code mappings remain preserved; no new dial push was performed. Source, binary, capture hashes and the report are retained locally under `artifacts/native-hardware-5b8b10d/sessions/endpoint-check/`. This confirms those two physical directions; it does not promote the experimental descriptor or qualify other firmware, variants, platforms, physical frame rate, dispatch latency or long-duration behavior.

On October 28, 2026, raw captures on the same macOS ARM64 `5548:1023` unit established vertical terminal codes in a human-confirmed up/down/up/down/left/right sequence. A quick-up regression then exposed a single-coordinate gesture that the initial displacement check dropped. The final bounded retest with codec SHA-256 `fa8855c21f34d9fcf69c2956243b2686033059dd68c6edbbbd6fae309fc955eb` recognized three quick upward flicks followed by down/left/right, with recorded modes/pages Control Bar → Button → Page 2 → Page 1. The user confirmed BAR appeared and the final BUTTON/PAGE 1 display; the intermediate Page 2 label was established by runtime state. The single production reader had a bounded raw-before-decode observer, all actions were disabled, all 22 reports were retained without drops, and shutdown was normal after 83.038 seconds. The [captured vertical fixture](fixtures/vertical-swipes.captured.json) retains report hashes, binary/source identities and this limited observation scope. Earlier failed and inconclusive attempts remain in local artifacts. Observer overhead is present; this provides no physical playback-rate or dispatch-latency measurement and does not qualify other firmware, variants or platforms.

The descriptor accepts PNG, JPEG, GIF and MP4 source assets. Rust decodes animated sources into timed PNG/JPEG transfers for keys, touch surfaces and backgrounds, using the same serialized output worker. GIF decoding is built in; MP4 desktop import uses browser decoding when supported, with optional FFmpeg fallback. Prepared profiles retain originals and frames and play without the decoder. Automated decoding and simulated transport tests do not verify the physical device's animation behavior.

A November 4, 2026 physical refresh investigation showed keys displaying uploaded images while STP was withheld for three seconds. STP therefore does not provide an atomic key presentation latch on that observed unit. Fixing macOS input/output lock contention reduced the ten-key diagnostic's median host transfer from 1731 ms to 47 ms, but short mixed-key intervals remained visible. This does not qualify other firmware or promote the experimental descriptor.

The subsequent lossless PNG output check reduced the normal-runtime full-scene median transfer from 76 ms to 25 ms, with approximately 65% fewer report bytes and additional host preparation work. Key, touch and background PNGs use the shared final-output compressor without changing pixel samples or refresh boundaries. The recording still shows brief mixed-display states; this is not an atomic presentation guarantee.

### Verification procedure

Run `cargo test -p stream-dock-manager-hardware-n4-pro` and `cargo run -p xtask -- hardware check --model n4-pro` first. Enumerate devices with `stream-dock-manager-cli devices`. `stream-dock-manager-cli listen --device <id>` starts the normal runtime and dispatches the active bindings while reporting input; it is not a passive listener. For physical input qualification, preseed a separate `--config /absolute/artifact/config` directory with a validated profile whose bindings all use `Action::None` before opening the device. A fresh default configuration includes browser and media actions and is not an inert test profile.

For each variant/firmware/platform combination:

1. Record USB identity, firmware from the vendor software, OS, architecture, desktop/session and driver revision.
2. Press/release each LCD key, rotate each encoder both ways, press every encoder and exercise all touch zones, coordinates and swipes. Compare stable control IDs and preserve edge/pulse semantics.
3. Check asymmetrical PNG/JPEG reference images for orientation, all slots, background rectangles, brightness, sleep and each knob color/off state.
   Test GIF and MP4 with recognizable frames on keys, touch surfaces and the full background. Check timing, orientation, looping, simultaneous surfaces and editor-hidden playback. Switch pages/profiles, replace animated media with static images, clear surfaces and sleep the device; confirm superseded animations stop. Disconnect/reconnect during playback and verify only the current profile returns.
4. Perform twenty unplug/replug cycles. Confirm queued images are discarded, current surfaces return and missed actions are never replayed.
5. Restart, sleep/wake, revoke/restore portal permission and change audio outputs. Verify independent device functions continue.
6. Connect two physical units and check input/output isolation. Run an eight-hour soak with representative plugins and the editor hidden.
7. Flood obsolete image updates; verify bounded memory, uninterrupted multipart transfers and no more than 20 surface transactions/second. Measure report-receipt-to-action dispatch; target less than 50 ms excluding external response time.

Use `stream-dock-manager-cli verify --help` to collect structured outcomes and evidence references. The command requires firmware, driver revision and a fresh output path. It never marks its own output reviewed. Review evidence before adding a record to `verification/results/`.

Explicit startup-logo upload uses the descriptor screen size and persistent `LOG` storage. Ordinary profile rendering never uses that command. Restart persistence and firmware acceptance remain physical checks.

</details>

[All hardware modules](../README.md) · [Manager setup](../../README.md)
