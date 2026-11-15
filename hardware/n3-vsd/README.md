# N3 (VSDinside / TreasLin)

Use this device with [Stream Dock Manager](../../README.md). Its adapter is included with the app; **no separate adapter download is needed**.

| At a glance                   | Details                                                                           |
| ----------------------------- | --------------------------------------------------------------------------------- |
| Model ID                      | `n3-vsd`                                                                          |
| Adapter status                | Experimental                                                                      |
| Registered controls           | 9 buttons, 3 encoders                                                             |
| Hardware testing              | Hardware tested on a user unit; full variant/platform qualification remains open. |
| Exact layout and capabilities | [Device descriptor](device.toml)                                                  |

Registered controls describe the Manager model, not a promise that every control or firmware has been physically verified. Dials are called encoders in the descriptor. Match your USB variant below before relying on a retail product name.

## Get started

1. [Install or build Manager](../../README.md).
2. Quit VSD Craft and other software that may be using the device.
3. Connect the device, open Manager, and select it.
4. Choose a profile, select a control that supports actions, assign an action, and save the binding.

You can explore the layout in the simulator without a connected device. On Linux, follow the [installation instructions](../../packaging/install-instructions/linux/INSTALL-INSTRUCTIONS.md) and reconnect after installing applicable USB access rules. Run Manager as your normal desktop user.

## Compatibility

This is the VSDinside / TreasLin variant, separate from the [N3 adapter](../n3/README.md). Match the USB identity before choosing a profile.

| Variant                   | USB vendor:product | Physical matching      |
| ------------------------- | ------------------ | ---------------------- |
| N3 (VSDinside / TreasLin) | `5548:1001`        | Enabled experimentally |

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
cargo test -p stream-dock-manager-hardware-n3-vsd
cargo run -p xtask -- hardware check --model n3-vsd
```

These checks validate source behavior and fixtures. For physical qualification, follow [verification/plan.toml](verification/plan.toml) on the exact unit, firmware, and platform. Simulated or replayed tests are not physical measurements.

<details>
<summary>Protocol notes and verification details</summary>

This is the six-LCD-key, three-plain-button, three-encoder N3 at **5548:1001**.
It is a separate model (`n3-vsd`), preserving existing Mirabox N3 descriptors and
profiles. Only interface 0, usage page 0xffa0, usage 1 matches. Its keyboard
interface is excluded. No other USB aliases are inferred.

Protocol facts are independently implemented from these pinned reference sources:

- [opendeck-akp03 mappings.rs](https://github.com/4ndv/opendeck-akp03/blob/10d183451807cc172fdf394bdd34f41a33fb52d3/src/mappings.rs): exact USB/page/usage, protocol v3, 64×64 JPEG with clockwise 90° rotation.
- [opendeck-akp03 inputs.rs](https://github.com/4ndv/opendeck-akp03/blob/10d183451807cc172fdf394bdd34f41a33fb52d3/src/inputs.rs): six display keys (1–6), three plain buttons (0x25/0x30/0x31), left/right/top encoders (press 0x33/0x34/0x35, turns 0x90/91, 0x60/61, 0x50/51).
- [mirajazz v0.16.2 device.rs](https://github.com/4ndv/mirajazz/blob/f0a7fa3f9b5eaaa9b699c667312336d9c56f6723/src/device.rs) and [state.rs](https://github.com/4ndv/mirajazz/blob/f0a7fa3f9b5eaaa9b699c667312336d9c56f6723/src/state.rs): ACK-prefixed input, byte 9 code / byte 10 state, v3 release edges, 1024-byte payloads plus HIDAPI report ID 0, DIS then LIG initialization, BAT image uploads and STP commit, CONNECT heartbeat, HAN sleep.

[Source hashes](protocol-sources.v1.json) identify inspected bytes. The pinned
manufacturer SDK does not register this variant; the additional reviewed
registration is recorded separately in `../additional-registrations.v1.json`.
Local macOS USB/HID enumeration observed the exact identity, interface 0,
512-byte input and 1024-byte output descriptors; serial numbers are omitted.
The rebuilt native runtime also opened this unit and reported it connected, using
an isolated temporary configuration. This bounded connection smoke test does not
qualify the control maps, display pixels, wake behavior or firmware. Synthetic fixtures
are source-derived, never captured reports. Enclosure geometry is schematic.

Only the six LCD keys receive image uploads. Plain buttons have no image slots.
The schematic places the six LCD keys at the left, with the three plain buttons
directly underneath and the encoders at the right.
The top LCD row aligns with the top encoder. Circular plain buttons sit centered
under each LCD column and show the compact default labels 1–3 in the preview.
Background, startup-logo writes, lighting remain unavailable
without exact-variant evidence. Firmware reads use the pinned mirajazz 20-byte feature report (ID 1) on macOS/Linux; Windows remains unsupported by that source. A 24 FPS host bound is not a physical rating.

Run `cargo test -p stream-dock-manager-hardware-n3-vsd -p stream-dock-manager-hardware-registry`
and `cargo run -p xtask -- hardware check --model n3-vsd`. Complete the model's
verification plan on the named unit/firmware/platform before claiming qualification.

</details>

[All hardware modules](../README.md) · [Manager setup](../../README.md)
