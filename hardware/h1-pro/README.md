# H1 Pro

This adapter is built into [Stream Dock Manager](../../README.md).

| At a glance             | Details                            |
| ----------------------- | ---------------------------------- |
| Model ID                | `h1-pro`                           |
| Adapter status          | Experimental                       |
| Controls                | 12 buttons                         |
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

| Variant            | USB vendor:product | Physical matching      |
| ------------------ | ------------------ | ---------------------- |
| H1 Pro (5548:1030) | `5548:1030`        | Enabled experimentally |
| H1 Pro (5548:1033) | `5548:1033`        | Enabled experimentally |

A matching USB ID lets Manager attempt a connection; it does not guarantee every feature on every firmware or OS. See the [device descriptor](device.toml) for exact interface selectors.

**Image output needs recognized firmware.** Manager enables images only after recognizing H1 V25/V3 firmware. If the version is missing or unknown, image output stays disabled with an explanation. See the protocol notes below for the source and limits.

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
cargo test -p stream-dock-manager-hardware-h1-pro
cargo run -p xtask -- hardware check --model h1-pro
```

These checks validate the adapter and its fixtures. For a physical test, follow the [verification plan](verification/plan.toml) and record the exact variant, firmware, and OS. A simulator or replay does not qualify hardware.

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
