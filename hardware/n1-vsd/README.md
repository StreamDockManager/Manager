# N1 (VSDinside / TreasLin)

Use this device with [Stream Dock Manager](../../README.md). Its adapter is included with the app; **no separate adapter download is needed**.

| At a glance                   | Details                                                                           |
| ----------------------------- | --------------------------------------------------------------------------------- |
| Model ID                      | `n1-vsd`                                                                          |
| Adapter status                | Experimental                                                                      |
| Registered controls           | 17 buttons, 3 displays, 1 encoder                                                 |
| Hardware testing              | Hardware tested on a user unit; full variant/platform qualification remains open. |
| Exact layout and capabilities | [Device descriptor](device.toml)                                                  |

Registered controls describe the Manager model, not a promise that every control or firmware has been physically verified. Dials are called encoders in the descriptor. Display-only surfaces do not accept action bindings. Match your USB variant below before relying on a retail product name.

## Get started

1. [Install or build Manager](../../README.md).
2. Quit VSD Craft and other software that may be using the device.
3. Connect the device, open Manager, and select it.
4. Choose a profile, select a control that supports actions, assign an action, and save the binding.

You can explore the layout in the simulator without a connected device. On Linux, follow the [installation instructions](../../packaging/install-instructions/linux/INSTALL-INSTRUCTIONS.md) and reconnect after installing applicable USB access rules. Run Manager as your normal desktop user.

## Compatibility

This is the VSDinside / TreasLin variant, separate from the [N1 adapter](../n1/README.md). Match the USB identity before choosing a profile.

| Variant                   | USB vendor:product | Physical matching      |
| ------------------------- | ------------------ | ---------------------- |
| N1 (VSDinside / TreasLin) | `5548:1002`        | Enabled experimentally |

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
cargo test -p stream-dock-manager-hardware-n1-vsd
cargo run -p xtask -- hardware check --model n1-vsd
```

These checks validate source behavior and fixtures. For physical qualification, follow [verification/plan.toml](verification/plan.toml) on the exact unit, firmware, and platform. Simulated or replayed tests are not physical measurements.

<details>
<summary>Protocol notes and verification details</summary>

This separate `n1-vsd` model matches **5548:1002**, interface 0, usage page
0xffa0, usage 1. It preserves the existing Mirabox N1 profiles and codec.
The keyboard interface is excluded.

The portrait preview has two plain buttons on the left and one dial at the top
right. A continuous display strip separates that row from fifteen 96×96 LCD
keys in three columns and five rows. The enclosure follows the reported dial-right
arrangement and remains schematic.

The strip has three separately addressable 64×64 image regions. Slots 1–15
belong to the main grid; slots 16/17 are `display-1`/`display-2`, and slot 18
remains `display-3`. The two physical buttons retain IDs `key-16`/`key-17`
and input codes 30/31; they have no screens on their caps. Their feedback links
preserve existing saved appearance bindings on the strip. The encoder retains
press/release/rotation and its slot-18 feedback link. JPEG images use no rotation
or mirroring.

The independently implemented codec follows these pinned public references:

- [opendeck-vsd-n1](https://github.com/tommasobbianchi/opendeck-vsd-n1/tree/635f1cb492c663e63200fcdaba9f436489f7a475): `src/mappings.rs`, `src/inputs.rs`, `src/device.rs`, and `docs/verified-mapping.md` establish the USB predicate, main-grid ordering, image-slot sizes, input maps and mode/wake sequence.
- [lightslinger](https://github.com/skyf/lightslinger/tree/a3d464243051f77627129c6b20981baf9df98310): mode constants and raw HID commands independently establish software-mode wire ASCII `3` for this identity.
- [mirajazz 0.16.2](https://github.com/4ndv/mirajazz/tree/f0a7fa3f9b5eaaa9b699c667312336d9c56f6723): the N1 reference's transport dependency establishes lazy DIS/LIG initialization, mode-before-wake ordering and ACK-only input framing in `src/device.rs` and `src/state.rs`.

The [manufacturer's product photos](https://www.vsdinside.com/products/vsdinside-stream-dock-n1-usa)
also distinguish the physical buttons from the continuous strip. The reference
plugin's rectangular grid presentation is not the preview's physical geometry.

[Source hashes](protocol-sources.v1.json) preserve inspected bytes. Upstream
reports of physical observations are third-party evidence, not Manager physical
qualification. The manufacturer app independently registers `VSDN1` at this USB
identity; [the retail source record](../retail-protocol-sources.v1.json) and
`python3 scripts/vsd_inventory.py '/Applications/VSD Craft.app'` verify its bytes
without executing vendor code.

Connection initialization sends **MOD ASCII 3 → wait 200 ms → DIS → LIG 0**, then clear/commit;
the runtime restores saved brightness afterwards. Ordinary wake commands send
**DIS** only, followed by the runtime's saved brightness restoration. The model's
`reinitialize_on_wake = false` keeps profile saves and scene restoration from
reselecting software mode or resetting brightness to zero. This follows the
reference plugin's connection-only mode selection, Mirajazz's one-time DIS/LIG
initialization, and Lightslinger's separate DIS wake command. These command
sequences remain source-derived; repeated physical sleep/wake qualification is
still required.

The connection-only settle interval follows a static inspection of the installed
Windows VSD Craft application. Both `SDActionCanvasWidget::onUsbDeviceConnected`
and `onDeviceConnected` select the N1 mode and schedule `refreshDisplay` with a
200 ms `QTimer`. In the executable, the USB-connected N1 branch calls mode setup
at RVA `0x1403f3` and passes `0xc8` milliseconds to the timer at RVA `0x14045d`;
the already-open-device path repeats this ordering at RVAs `0x1346e3` and
`0x134741`. Manager's model-owned initialization plan places the wait after the
mode write completes and before its remaining setup and image restoration.
The byte-only `initialize()` contract is unchanged; connection consumers use
`initialization_plan()` to retain the timing. Ordinary wakes and frame uploads
do not repeat the interval.

Inspected file identities (SHA-256; vendor code was not executed):

| File             | File version    | SHA-256                                                            |
| ---------------- | --------------- | ------------------------------------------------------------------ |
| `VSD Craft.exe`  | `3.10.205.0918` | `a60489d22ab51a93ea54370173eaf751498dfece35e425a25527b7888b767deb` |
| `SDLibrary1.dll` | `1.0.25.0609`   | `ac93fb1c1f4de5ccec08545ba8d702fd2376a4d5db3395b6194895c268580d00` |

The DLL's wired N1 icon path writes BAT followed by report-ID-prefixed image
chunks without a per-chunk delay or firmware acknowledgement gate. Its
NearLink pacing and legacy-model acknowledgement waits are separate paths.
These static observations establish source provenance, not a physical rendering
result; reconnect qualification on the attached firmware remains required.

Shared CRT `mode = 2` is a zero-based offset (`b'1' + 2`), not a request to send
wire ASCII `2`. The existing
runtime sends CONNECT every ten seconds, within the upstream reported roughly
35-second disconnect interval. Input requires ACK framing and a known control
code, without the additional OK marker required by the manufacturer SDK family.
Write confirmations and unknown input codes do not dispatch actions. The first
valid input is delivered without priming.

Background, startup-logo storage, lighting remain
unavailable for this variant. Firmware reads use the pinned mirajazz 20-byte feature report (ID 1) on macOS/Linux; Windows remains unsupported by that source. A 24 FPS host bound is not a physical rating.
The synthetic fixture is reconstructed from protocol facts, never a HID capture.
No locally reviewed physical results are bundled.

Run the module tests and `cargo run -p xtask -- hardware check --model n1-vsd`.
Complete the model verification plan on the named unit/firmware/platform before
claiming physical qualification.

</details>

[All hardware modules](../README.md) · [Manager setup](../../README.md)
