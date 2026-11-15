# H1 Pro

Use this device with [Stream Dock Manager](../../README.md). Its adapter is included with the app; **no separate adapter download is needed**.

| At a glance                   | Details                                          |
| ----------------------------- | ------------------------------------------------ |
| Model ID                      | `h1-pro`                                         |
| Adapter status                | Experimental                                     |
| Registered controls           | 12 buttons                                       |
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

| Variant            | USB vendor:product | Physical matching      |
| ------------------ | ------------------ | ---------------------- |
| H1 Pro (5548:1030) | `5548:1030`        | Enabled experimentally |
| H1 Pro (5548:1033) | `5548:1033`        | Enabled experimentally |

All variants remain **experimental**. Enabled matching is not a claim of full firmware, platform, or hardware qualification. Exact interface/usage selectors and capabilities are defined in [device.toml](device.toml).

Some source-derived interfaces intentionally have no automatic Linux permission grant. Do not work around this by making HID devices world-writable.

**Image output needs recognized firmware.** Manager enables images only after recognizing H1 V25/V3 firmware. If the version is missing or unknown, image output stays disabled with an explanation. See the protocol notes below for the source and limits.

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
cargo test -p stream-dock-manager-hardware-h1-pro
cargo run -p xtask -- hardware check --model h1-pro
```

These checks validate source behavior and fixtures. For physical qualification, follow [verification/plan.toml](verification/plan.toml) on the exact unit, firmware, and platform. Simulated or replayed tests are not physical measurements.

<details>
<summary>Protocol notes and verification details</summary>

Descriptor and codec facts are pinned to the vendor StreamDockH1Pro source at `fc124cb5bc8fe0cd6c72d3f2af80588adc7dcffb`. See [shared protocol provenance](../protocol-evidence.md) for source hashes, framing, selectors and limits.

The synthetic fixture is source-derived and is not captured/physical evidence. No firmware/platform/variant is physically qualified. The preview is schematic; only image pixel dimensions and protocol control order come from the pinned source.

Retired schematic IDs: none. Bindings on retired IDs remain readable and inactive until an explicit conversion mapping/omission. The new profile copy preserves its source and device assignments.

USB matching requires the exact vendor/product and usage 1 on usage pages greater than 1025, as the vendor enumeration does. Its source does not establish an exact interface. No broad automatic Linux permission grant is generated; interface/permission qualification remains pending.

H1 Pro initializes key mode and reads firmware before enabling image output. Recognized H1 V25/V3 firmware selects 64×64 keys rotated counterclockwise. Missing or unrecognized firmware disables image output with an explanation; the SDK catchall is not a positive identifier for legacy 70×70 hardware. Boot upload, device-stored animation and whole-panel screensaver modes remain excluded. Imported animations use host-driven frames only.

Run the module tests and `cargo run -p xtask -- hardware check --model h1-pro`. Populate reviewed physical records only after completing the model plan on the named unit/firmware/session.

</details>

Current image/input/boot contracts and firmware restrictions are in the [revision-pinned codec matrix](../protocol-audit.md#revision-pinned-model-variant-and-firmware-matrix). All matching remains experimental; no physical results were produced by this refresh.

[All hardware modules](../README.md) · [Manager setup](../../README.md)
