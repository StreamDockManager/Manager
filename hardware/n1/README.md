# N1

Use this device with [Stream Dock Manager](../../README.md). Its adapter is included with the app; **no separate adapter download is needed**.

| At a glance                   | Details                                          |
| ----------------------------- | ------------------------------------------------ |
| Model ID                      | `n1`                                             |
| Adapter status                | Experimental                                     |
| Registered controls           | 17 buttons, 1 encoder, 3 displays                |
| Hardware testing              | No Manager hardware-tested claim for this model. |
| Exact layout and capabilities | [Device descriptor](device.toml)                 |

Registered controls describe the Manager model, not a promise that every control or firmware has been physically verified. Dials are called encoders in the descriptor. Display-only surfaces do not accept action bindings. Match your USB variant below before relying on a retail product name.

## Get started

1. [Install or build Manager](../../README.md).
2. Quit VSD Craft and other software that may be using the device.
3. Connect the device, open Manager, and select it.
4. Choose a profile, select a control that supports actions, assign an action, and save the binding.

You can explore the layout in the simulator without a connected device. On Linux, follow the [installation instructions](../../packaging/install-instructions/linux/INSTALL-INSTRUCTIONS.md) and reconnect after installing applicable USB access rules. Run Manager as your normal desktop user.

## Compatibility

Looking for the VSDinside / TreasLin model? Use the separate [N1 VSD guide](../n1-vsd/README.md). Match the USB identity rather than the product name.

| Variant        | USB vendor:product | Physical matching      |
| -------------- | ------------------ | ---------------------- |
| N1 (6603:1011) | `6603:1011`        | Enabled experimentally |
| N1 (6603:1000) | `6603:1000`        | Enabled experimentally |

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
cargo test -p stream-dock-manager-hardware-n1
cargo run -p xtask -- hardware check --model n1
```

These checks validate source behavior and fixtures. For physical qualification, follow [verification/plan.toml](verification/plan.toml) on the exact unit, firmware, and platform. Simulated or replayed tests are not physical measurements.

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
