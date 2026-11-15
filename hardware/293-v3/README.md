# 293 V3

Use this device with [Stream Dock Manager](../../README.md). Its adapter is included with the app; **no separate adapter download is needed**.

| At a glance                   | Details                                          |
| ----------------------------- | ------------------------------------------------ |
| Model ID                      | `293-v3`                                         |
| Adapter status                | Experimental                                     |
| Registered controls           | 15 buttons                                       |
| Hardware testing              | No Manager hardware-tested claim for this model. |
| Exact layout and capabilities | [Device descriptor](device.toml)                 |

Registered controls describe the Manager model, not a promise that every control or firmware has been physically verified. Match your USB variant below before relying on a retail product name.

## Get started

1. [Install or build Manager](../../README.md).
2. Quit VSD Craft and other software that may be using the device.
3. Connect the device, open Manager, and select it.
4. Choose a profile, select a control that supports actions, assign an action, and save the binding.

You can explore the layout in the simulator without a connected device. On Linux, follow the [installation instructions](../../packaging/install-instructions/linux/INSTALL-INSTRUCTIONS.md) and reconnect after installing applicable USB access rules. Run Manager as your normal desktop user.

## Compatibility

| Variant                        | USB vendor:product | Physical matching      |
| ------------------------------ | ------------------ | ---------------------- |
| 293 V3 (6603:1005)             | `6603:1005`        | Enabled experimentally |
| 293 V3 (6603:1006)             | `6603:1006`        | Enabled experimentally |
| 293 V3 (6603:1010)             | `6603:1010`        | Enabled experimentally |
| HSV293S (VSDinside / TreasLin) | `5548:1003`        | Enabled experimentally |

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
cargo test -p stream-dock-manager-hardware-293-v3
cargo run -p xtask -- hardware check --model 293-v3
```

These checks validate source behavior and fixtures. For physical qualification, follow [verification/plan.toml](verification/plan.toml) on the exact unit, firmware, and platform. Simulated or replayed tests are not physical measurements.

<details>
<summary>Protocol notes and verification details</summary>

The TreasLin/VSDinside **HSV293S** retail variant at **5548:1003** is also
registered here as `VSD293V3`. This is distinct from the disputed older
`5548:6670` 293s; that source gate remains in place. VSD Craft's pinned identity
table and shared 293V3 layout/mapping/packet paths establish this registration.
See [retail protocol evidence](../retail-protocol-sources.v1.json). Verify the
manufacturer bytes with `python3 scripts/vsd_inventory.py '/Applications/VSD Craft.app'`.
No interface number is inferred, so no broad Linux permission grant is added.
This variant has not been physically qualified with Manager.

Descriptor and codec facts are pinned to the vendor StreamDock293V3 source at `fc124cb5bc8fe0cd6c72d3f2af80588adc7dcffb`. See [shared protocol provenance](../protocol-evidence.md) for source hashes, framing, selectors and limits.

The synthetic fixture is source-derived and is not captured/physical evidence. No firmware/platform/variant is physically qualified. The preview is schematic; only image pixel dimensions and protocol control order come from the pinned source. Encoder presses remain pulses, with no claimed release capability.

Retired schematic IDs: none. Bindings on retired IDs remain readable and inactive until an explicit conversion mapping/omission. The new profile copy preserves its source and device assignments.

USB matching requires the exact vendor/product and usage 1 on usage pages greater than 1025, as the vendor enumeration does. Its source does not establish an exact interface. No broad automatic Linux permission grant is generated; interface/permission qualification remains pending.

Run the module tests and `cargo run -p xtask -- hardware check --model 293-v3`. Populate reviewed physical records only after completing the model plan on the named unit/firmware/session.

Explicit startup-logo upload uses the descriptor screen size and persistent `LOG` storage. Ordinary profile rendering never uses that command. Restart persistence and firmware acceptance remain physical checks.

</details>

Current image/input/boot contracts and firmware restrictions are in the [revision-pinned codec matrix](../protocol-audit.md#revision-pinned-model-variant-and-firmware-matrix). All matching remains experimental; no physical results were produced by this refresh.

[All hardware modules](../README.md) · [Manager setup](../../README.md)
