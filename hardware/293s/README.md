# 293s

This adapter is built into [Stream Dock Manager](../../README.md).

| At a glance             | Details                            |
| ----------------------- | ---------------------------------- |
| Model ID                | `293s`                             |
| Adapter status          | Experimental                       |
| Controls                | 15 buttons, 3 displays             |
| Hardware testing        | Not physically verified by Manager |
| Layout and capabilities | [Device descriptor](device.toml)   |

Match the [USB ID](#compatibility), not just the product name. Display-only surfaces do not accept action bindings.

## Connect and assign an action

1. [Install Manager](../../README.md) and close other dock software, such as VSD Craft.
2. Connect your dock. Open Manager and select the device and a profile in **Device editor**.
3. Select a control, choose an action, and save the binding.

- **No hardware?** Select this model in the simulator.
- **Linux:** Follow the [USB access setup](../../packaging/install-instructions/linux/INSTALL-INSTRUCTIONS.md), then reconnect. Run as your normal desktop user and do not broaden HID permissions.

## Compatibility

| Variant          | USB vendor:product | Physical matching      |
| ---------------- | ------------------ | ---------------------- |
| 293s (5548:6670) | `5548:6670`        | Enabled experimentally |

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
cargo test -p stream-dock-manager-hardware-293s
cargo run -p xtask -- hardware check --model 293s
```

These checks validate the adapter and its fixtures. For a physical test, follow the [verification plan](verification/plan.toml) and record the exact variant, firmware, and OS. A simulator or replay does not qualify hardware.

<details>
<summary>Protocol notes and verification details</summary>

Descriptor and codec facts are pinned to the vendor StreamDock293s source at `fc124cb5bc8fe0cd6c72d3f2af80588adc7dcffb`. See [shared protocol provenance](../protocol-evidence.md) for source hashes, framing, selectors and limits.

The synthetic fixture is source-derived and is not captured/physical evidence. No firmware/platform/variant is physically qualified. The preview is schematic; only image pixel dimensions and protocol control order come from the pinned source. Encoder presses remain pulses, with no claimed release capability.

Stable IDs `key-16`, `key-17` and `key-18` now describe display-only surfaces. Saved appearances and profiles remain readable; old button bindings on those IDs are inactive and are not advertised as SDK action contexts. No input is invented for their image slots.

USB matching requires the exact vendor/product and usage 1 on usage pages greater than 1025, as the vendor enumeration does. Its source does not establish an exact interface. No broad automatic Linux permission grant is generated; interface/permission qualification remains pending.

Run the module tests and `cargo run -p xtask -- hardware check --model 293s`. Populate reviewed physical records only after completing the model plan on the named unit/firmware/session.

Explicit startup-logo upload uses the descriptor screen size and persistent `LOG` storage. Ordinary profile rendering never uses that command. Restart persistence and firmware acceptance remain physical checks.

</details>

Current image/input/boot contracts and firmware restrictions are in the [revision-pinned codec matrix](../protocol-audit.md#revision-pinned-model-variant-and-firmware-matrix). All matching remains experimental; no physical results were produced by this refresh.

[All hardware modules](../README.md) · [Manager setup](../../README.md)
