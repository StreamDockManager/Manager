# XL

This adapter is built into [Stream Dock Manager](../../README.md).

| At a glance             | Details                            |
| ----------------------- | ---------------------------------- |
| Model ID                | `xl`                               |
| Adapter status          | Experimental                       |
| Controls                | 38 buttons                         |
| Hardware testing        | Not physically verified by Manager |
| Layout and capabilities | [Device descriptor](device.toml)   |

Match the [USB ID](#compatibility), not just the product name.

## Connect and assign an action

1. [Install Manager](../../README.md) and close other dock software, such as VSD Craft.
2. Connect your dock. Open Manager and select the device and a profile in **Device editor**.
3. Select a control, choose an action, and save the binding.

- **No hardware?** Select this model in the simulator.
- **Linux:** Follow the [USB access setup](../../packaging/install-instructions/linux/INSTALL-INSTRUCTIONS.md), then reconnect. Run as your normal desktop user and do not broaden HID permissions.

## Compatibility

| Variant                   | USB vendor:product | Physical matching      |
| ------------------------- | ------------------ | ---------------------- |
| XL (5548:1028)            | `5548:1028`        | Enabled experimentally |
| XL (5548:1031)            | `5548:1031`        | Enabled experimentally |
| XL (VSDinside / TreasLin) | `5548:1034`        | Enabled experimentally |

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
cargo test -p stream-dock-manager-hardware-xl
cargo run -p xtask -- hardware check --model xl
```

These checks validate the adapter and its fixtures. For a physical test, follow the [verification plan](verification/plan.toml) and record the exact variant, firmware, and OS. A simulator or replay does not qualify hardware.

<details>
<summary>Protocol notes and verification details</summary>

The TreasLin/VSDinside **XL** at **5548:1034** is also registered here as
`VSDXL`. VSD Craft's pinned identity and origin-type tables map it to the same
XL origin type as the existing `5548:1028` and `5548:1031` family. See
[retail protocol evidence](../retail-protocol-sources.v1.json). Verify the
manufacturer bytes with `python3 scripts/vsd_inventory.py '/Applications/VSD Craft.app'`.
The existing 32 image keys and two three-position switches retain their IDs;
no encoder or new codec is inferred from retail descriptions of scroll wheels.
No interface number is inferred, so no broad Linux permission grant is added.
This variant has not been physically qualified with Manager.

Descriptor and codec facts are pinned to the vendor StreamDockXL source at `fc124cb5bc8fe0cd6c72d3f2af80588adc7dcffb`. See [shared protocol provenance](../protocol-evidence.md) for source hashes, framing, selectors and limits.

The synthetic fixture is source-derived and is not captured/physical evidence. No firmware/platform/variant is physically qualified. The preview is schematic; only image pixel dimensions and protocol control order come from the pinned source. Encoder presses remain pulses, with no claimed release capability.

Retired schematic IDs: none. Bindings on retired IDs remain readable and inactive until an explicit conversion mapping/omission. The new profile copy preserves its source and device assignments.

USB matching requires the exact vendor/product and usage 1 on usage pages greater than 1025, as the vendor enumeration does. Its source does not establish an exact interface. No broad automatic Linux permission grant is generated; interface/permission qualification remains pending.

Run the module tests and `cargo run -p xtask -- hardware check --model xl`. Populate reviewed physical records only after completing the model plan on the named unit/firmware/session.

Explicit startup-logo upload uses the descriptor screen size and persistent `LOG` storage. Ordinary profile rendering never uses that command. Restart persistence and firmware acceptance remain physical checks.

</details>

Current image/input/boot contracts and firmware restrictions are in the [revision-pinned codec matrix](../protocol-audit.md#revision-pinned-model-variant-and-firmware-matrix). All matching remains experimental; no physical results were produced by this refresh.

[All hardware modules](../README.md) · [Manager setup](../../README.md)
