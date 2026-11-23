# N4

This adapter is built into [Stream Dock Manager](../../README.md).

| At a glance             | Details                               |
| ----------------------- | ------------------------------------- |
| Model ID                | `n4`                                  |
| Adapter status          | Experimental                          |
| Controls                | 14 buttons, 4 encoders, 1 touch strip |
| Hardware testing        | Not physically verified by Manager    |
| Layout and capabilities | [Device descriptor](device.toml)      |

Match the [USB ID](#compatibility), not just the product name. Dials are called encoders in the descriptor.

## Connect and assign an action

1. [Install Manager](../../README.md) and close other dock software, such as VSD Craft.
2. Connect your dock. Open Manager and select the device and a profile in **Device editor**.
3. Select a control, choose an action, and save the binding.

In the editor, the four lower LCD sections form one touch panel. Each section
retains its own press/release binding and can display feedback from its paired
dial. Assign the strip's left and right swipes separately. N4 does not expose
N4 Pro coordinate tracking, vertical swipes, or a shared live background.

- **No hardware?** Select this model in the simulator.
- **Linux:** Follow the [USB access setup](../../packaging/install-instructions/linux/INSTALL-INSTRUCTIONS.md), then reconnect. Run as your normal desktop user and do not broaden HID permissions.

## Compatibility

| Variant        | USB vendor:product | Physical matching      |
| -------------- | ------------------ | ---------------------- |
| N4 (6602:1001) | `6602:1001`        | Enabled experimentally |
| N4 (6603:1007) | `6603:1007`        | Enabled experimentally |

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
cargo test -p stream-dock-manager-hardware-n4
cargo run -p xtask -- hardware check --model n4
```

These checks validate the adapter and its fixtures. For a physical test, follow the [verification plan](verification/plan.toml) and record the exact variant, firmware, and OS. A simulator or replay does not qualify hardware.

<details>
<summary>Protocol notes and verification details</summary>

Descriptor and codec facts are pinned to the vendor StreamDockN4 source at `fc124cb5bc8fe0cd6c72d3f2af80588adc7dcffb`. See [shared protocol provenance](../protocol-evidence.md) for source hashes, framing, selectors and limits.

The synthetic fixture is source-derived and is not captured/physical evidence. No firmware/platform/variant is physically qualified. The preview is schematic; only image pixel dimensions and protocol control order come from the pinned source. Encoder presses expose strict press/release edges; existing pulse-style actions still run once per press. Physical release reliability remains unqualified.

Retired schematic IDs: touch-1, touch-2, touch-3, touch-4. Bindings on retired IDs remain readable and inactive until an explicit conversion mapping/omission. The new profile copy preserves its source and device assignments.

USB matching requires the exact vendor/product and usage 1 on usage pages greater than 1025, as the vendor enumeration does. Its source does not establish an exact interface. No broad automatic Linux permission grant is generated; interface/permission qualification remains pending.

N4 swipe labels follow its pinned model decoder. The panel groups the existing
`aux-key-1` through `aux-key-4` controls with `touch-strip` for editing, but
does not reinterpret their press/release edges as raw touch coordinates or
rename any saved binding. N4 Pro trajectory observations are not transferred
to this model.

Run the module tests and `cargo run -p xtask -- hardware check --model n4`. Populate reviewed physical records only after completing the model plan on the named unit/firmware/session.

Explicit startup-logo upload uses the descriptor screen size, a sourced 180° rotation without the live-background horizontal flip, and persistent `LOG` storage. Ordinary profile rendering never uses that command. Restart persistence and firmware acceptance remain physical checks.

</details>

Current image/input/boot contracts and firmware restrictions are in the [revision-pinned codec matrix](../protocol-audit.md#revision-pinned-model-variant-and-firmware-matrix). All matching remains experimental; no physical results were produced by this refresh.

[All hardware modules](../README.md) · [Manager setup](../../README.md)
