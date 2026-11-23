# N1

This adapter is built into [Stream Dock Manager](../../README.md).

| At a glance             | Details                            |
| ----------------------- | ---------------------------------- |
| Model ID                | `n1`                               |
| Adapter status          | Experimental                       |
| Controls                | 17 buttons, 1 encoder, 3 displays  |
| Hardware testing        | Not physically verified by Manager |
| Layout and capabilities | [Device descriptor](device.toml)   |

Match the [USB ID](#compatibility), not just the product name. Dials are called encoders in the descriptor. Display-only surfaces do not accept action bindings.

## Connect and assign an action

1. [Install Manager](../../README.md) and close other dock software, such as VSD Craft.
2. Connect your dock. Open Manager and select the device and a profile in **Device editor**.
3. Select a control, choose an action, and save the binding.

- **No hardware?** Select this model in the simulator.
- **Linux:** Follow the [USB access setup](../../packaging/install-instructions/linux/INSTALL-INSTRUCTIONS.md), then reconnect. Run as your normal desktop user and do not broaden HID permissions.

## Compatibility

Looking for the VSDinside / TreasLin model? Use the separate [N1 VSD guide](../n1-vsd/README.md). Match the USB identity rather than the product name.

| Variant        | USB vendor:product | Physical matching      |
| -------------- | ------------------ | ---------------------- |
| N1 (6603:1011) | `6603:1011`        | Enabled experimentally |
| N1 (6603:1000) | `6603:1000`        | Enabled experimentally |

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
cargo test -p stream-dock-manager-hardware-n1
cargo run -p xtask -- hardware check --model n1
```

These checks validate the adapter and its fixtures. For a physical test, follow the [verification plan](verification/plan.toml) and record the exact variant, firmware, and OS. A simulator or replay does not qualify hardware.

<details>
<summary>Protocol notes and verification details</summary>

Descriptor and codec facts are pinned to the vendor StreamDockN1 source at `fc124cb5bc8fe0cd6c72d3f2af80588adc7dcffb`. See [shared protocol provenance](../protocol-evidence.md) for source hashes, framing, selectors and limits.

The synthetic fixture is source-derived and is not captured/physical evidence. No firmware/platform/variant is physically qualified. The preview is schematic; only image pixel dimensions and protocol control order come from the pinned source. Encoder presses remain pulses, with no claimed release capability.

Retired schematic IDs: key-18. Bindings on retired IDs remain readable and inactive until an explicit conversion mapping/omission. The new profile copy preserves its source and device assignments.

USB matching requires the exact vendor/product and usage 1 on usage pages greater than 1025, as the vendor enumeration does. Its source does not establish an exact interface. No broad automatic Linux permission grant is generated; interface/permission qualification remains pending.

N1 background requires a firmware threshold that is not established by a USB release number, so it remains unavailable. The portrait preview follows the reported N1 arrangement: two plain buttons on the left, a dial at the top right, a continuous strip underneath, and a three-column, five-row LCD grid below.

The strip comprises three independently addressable 80×80 image regions. `display-1`/`display-2` own image slots 16/17, and `display-3` retains slot 18. The plain buttons keep IDs `key-16`/`key-17` and input codes 30/31, with feedback links to the strip that preserve existing saved appearance bindings. The dial keeps its pulse semantics and gains an explicit link to the slot-18 feedback region. The official SDK retains 80×80 source sizes, wire addresses and stable IDs. The independent retail N1-VSD uses 64×64 regions and remains a separate codec.

Run the module tests and `cargo run -p xtask -- hardware check --model n1`. Populate reviewed physical records only after completing the model plan on the named unit/firmware/session.

</details>

Current image/input/boot contracts and firmware restrictions are in the [revision-pinned codec matrix](../protocol-audit.md#revision-pinned-model-variant-and-firmware-matrix). All matching remains experimental; no physical results were produced by this refresh.

[All hardware modules](../README.md) · [Manager setup](../../README.md)
