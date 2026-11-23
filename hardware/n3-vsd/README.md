# N3 (VSDinside / TreasLin)

This adapter is built into [Stream Dock Manager](../../README.md).

| At a glance             | Details                                                               |
| ----------------------- | --------------------------------------------------------------------- |
| Model ID                | `n3-vsd`                                                              |
| Adapter status          | Experimental                                                          |
| Controls                | 9 buttons, 3 encoders                                                 |
| Hardware testing        | Tested on a user unit; results do not cover every variant or platform |
| Layout and capabilities | [Device descriptor](device.toml)                                      |

Match the [USB ID](#compatibility), not just the product name. Dials are called encoders in the descriptor.

## Connect and assign an action

1. [Install Manager](../../README.md) and close other dock software, such as VSD Craft.
2. Connect your dock. Open Manager and select the device and a profile in **Device editor**.
3. Select a control, choose an action, and save the binding.

- **No hardware?** Select this model in the simulator.
- **Linux:** Follow the [USB access setup](../../packaging/install-instructions/linux/INSTALL-INSTRUCTIONS.md), then reconnect. Run as your normal desktop user and do not broaden HID permissions.

## Compatibility

This is the VSDinside / TreasLin variant, separate from the [N3 adapter](../n3/README.md). Match the USB identity before choosing a profile.

| Variant                   | USB vendor:product | Physical matching      |
| ------------------------- | ------------------ | ---------------------- |
| N3 (VSDinside / TreasLin) | `5548:1001`        | Enabled experimentally |

A matching USB ID lets Manager attempt a connection; it does not guarantee every feature on every firmware or OS. See the [device descriptor](device.toml) for exact interface selectors.

## Troubleshooting

| Problem                  | Check                                                                                                         |
| ------------------------ | ------------------------------------------------------------------------------------------------------------- |
| Device missing           | Close other dock apps, reconnect USB and match the USB ID above.                                              |
| Linux access denied      | Use the linked USB setup. Some experimental interfaces have no automatic permission rule.                     |
| Layout or image differs  | Confirm the exact variant in [device.toml](device.toml); similarly named devices can use different protocols. |
| Unknown firmware version | See [firmware checks](../README.md#firmware-checks). An unknown version is not an up-to-date result.          |

## For contributors

From the **Manager repository root**, after setting up the [contributor prerequisites](../../CONTRIBUTING.md):

```sh
python3 scripts/prepare_patched_deps.py
cargo test -p stream-dock-manager-hardware-n3-vsd
cargo run -p xtask -- hardware check --model n3-vsd
```

These checks validate the adapter and its fixtures. For a physical test, follow the [verification plan](verification/plan.toml) and record the exact variant, firmware, and OS. A simulator or replay does not qualify hardware.

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
