# N3

This adapter is built into [Stream Dock Manager](../../README.md).

| At a glance             | Details                            |
| ----------------------- | ---------------------------------- |
| Model ID                | `n3`                               |
| Adapter status          | Experimental                       |
| Controls                | 9 buttons, 3 encoders              |
| Hardware testing        | Not physically verified by Manager |
| Layout and capabilities | [Device descriptor](device.toml)   |

Match the [USB ID](#compatibility), not just the product name. Dials are called encoders in the descriptor.

## Connect and assign an action

1. [Install Manager](../../README.md) and close other dock software, such as VSD Craft.
2. Connect your dock. Open Manager and select the device and a profile in **Device editor**.
3. Select a control, choose an action, and save the binding.

- **No hardware?** Select this model in the simulator.
- **Linux:** Follow the [USB access setup](../../packaging/install-instructions/linux/INSTALL-INSTRUCTIONS.md), then reconnect. Run as your normal desktop user and do not broaden HID permissions.

## Compatibility

Looking for the VSDinside / TreasLin model? Use the separate [N3 VSD guide](../n3-vsd/README.md). Match the USB identity rather than the product name.

| Variant        | USB vendor:product | Physical matching      |
| -------------- | ------------------ | ---------------------- |
| N3 (6602:1000) | `6602:1000`        | Enabled experimentally |
| N3 (6603:1002) | `6603:1002`        | Enabled experimentally |
| N3 (6603:1003) | `6603:1003`        | Enabled experimentally |
| N3 (6602:1002) | `6602:1002`        | Enabled experimentally |
| N3 (6602:1003) | `6602:1003`        | Enabled experimentally |
| N3 (6602:2929) | `6602:2929`        | Enabled experimentally |
| N3 (1500:3001) | `1500:3001`        | Enabled experimentally |

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
cargo test -p stream-dock-manager-hardware-n3
cargo run -p xtask -- hardware check --model n3
```

These checks validate the adapter and its fixtures. For a physical test, follow the [verification plan](verification/plan.toml) and record the exact variant, firmware, and OS. A simulator or replay does not qualify hardware.

<details>
<summary>Protocol notes and verification details</summary>

Descriptor and codec facts are pinned to the vendor StreamDockN3 source at `fc124cb5bc8fe0cd6c72d3f2af80588adc7dcffb`. See [shared protocol provenance](../protocol-evidence.md) for source hashes, framing, selectors and limits.

The synthetic fixture is source-derived and is not captured/physical evidence. No firmware/platform/variant is physically qualified. The preview is schematic; only image pixel dimensions and protocol control order come from the pinned source. Encoder presses expose strict press/release edges; existing pulse-style actions still run once per press. Physical release reliability remains unqualified.

Retired schematic IDs: key-10, key-11, key-12, key-13, key-14, key-15, key-16, key-17, key-18. Bindings on retired IDs remain readable and inactive until an explicit conversion mapping/omission. The new profile copy preserves its source and device assignments.

USB matching requires the exact vendor/product and usage 1 on usage pages greater than 1025, as the vendor enumeration does. Its source does not establish an exact interface. No broad automatic Linux permission grant is generated; interface/permission qualification remains pending.

Run the module tests and `cargo run -p xtask -- hardware check --model n3`. Populate reviewed physical records only after completing the model plan on the named unit/firmware/session.

Six LCD controls (`key-1`–`key-6`) accept images; `key-7`–`key-9` remain plain buttons. Existing bindings and artwork data are retained. Legacy `6602` variants use 60×60 unrotated JPEG keys and clockwise-rotated RGB565 little-endian boot output (153,600 bytes). Newer `6603` and `1500` variants use 64×64 clockwise-rotated JPEG keys and JPEG boot output. Explicit startup-logo upload uses the 320×240 input canvas and persistent `LOG` storage. Ordinary profile rendering never uses that command. Restart persistence and firmware acceptance remain physical checks.

</details>

Current image/input/boot contracts and firmware restrictions are in the [revision-pinned codec matrix](../protocol-audit.md#revision-pinned-model-variant-and-firmware-matrix). All matching remains experimental; no physical results were produced by this refresh.

[All hardware modules](../README.md) · [Manager setup](../../README.md)
