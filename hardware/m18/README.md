# M18

Explore the M18 layout in the [Stream Dock Manager](../../README.md) simulator. **Physical M18 support is currently disabled**; installing Manager does not enable it.

| At a glance                   | Details                                          |
| ----------------------------- | ------------------------------------------------ |
| Model ID                      | `m18`                                            |
| Adapter status                | Experimental; physical matching disabled         |
| Registered controls           | 18 buttons                                       |
| Hardware testing              | No Manager hardware-tested claim for this model. |
| Exact layout and capabilities | [Device descriptor](device.toml)                 |

Registered controls describe the simulator model, not verified physical support. The USB identities below document the variants that remain disabled.

## Get started

Use the simulator to explore the layout and prepare a profile:

1. [Install or build Manager](../../README.md).
2. Open the simulator and choose M18.
3. Choose a profile, assign actions to compatible controls, and save your bindings.

## Compatibility

| Variant         | USB vendor:product | Physical matching |
| --------------- | ------------------ | ----------------- |
| M18 (6603:1009) | `6603:1009`        | Disabled          |
| M18 (6603:1012) | `6603:1012`        | Disabled          |
| M18 (5548:1000) | `5548:1000`        | Disabled          |

- Firmware-dependent background and lighting modes differ in the vendor implementations. The connected firmware mode is not established.

All variants remain **experimental and disabled for physical matching**. Exact interface/usage selectors and capabilities are defined in [device.toml](device.toml).

Some source-derived interfaces intentionally have no automatic Linux permission grant. Do not work around this by making HID devices world-writable.

## If it does not work

| Question                                 | Answer                                                                                                  |
| ---------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| Why is my connected M18 not listed?      | Physical matching is disabled for every variant above. Use the simulator.                               |
| Will changing USB permissions enable it? | No. Do not broaden device permissions or run Manager as root.                                           |
| Can I prepare a profile?                 | Yes, with the M18 simulator. It does not establish physical input or display behavior.                  |
| Can Manager read its firmware?           | Not while physical matching is disabled. See [firmware-read limitations](../README.md#firmware-checks). |

## For contributors

From the **Manager repository root**, after setting up the [contributor prerequisites](../../CONTRIBUTING.md):

```sh
python3 scripts/prepare_patched_deps.py
cargo test -p stream-dock-manager-hardware-m18
cargo run -p xtask -- hardware check --model m18
```

These checks validate source behavior and fixtures. For physical qualification, follow [verification/plan.toml](verification/plan.toml) on the exact unit, firmware, and platform. Simulated or replayed tests are not physical measurements.

<details>
<summary>Protocol notes and verification details</summary>

Descriptor and codec facts are pinned to the vendor StreamDockM18 source at `fc124cb5bc8fe0cd6c72d3f2af80588adc7dcffb`. See [shared protocol provenance](../protocol-evidence.md) for source hashes, framing, selectors and limits.

The synthetic fixture is source-derived and is not captured/physical evidence. No firmware/platform/variant is physically qualified. The preview is schematic; only image pixel dimensions and protocol control order come from the pinned source.

Retired schematic IDs: none. Bindings on retired IDs remain readable and inactive until an explicit conversion mapping/omission. The new profile copy preserves its source and device assignments.

USB matching requires the exact vendor/product and usage 1 on usage pages greater than 1025, as the vendor enumeration does. Its source does not establish an exact interface. No broad automatic Linux permission grant is generated; interface/permission qualification remains pending.

Run the module tests and `cargo run -p xtask -- hardware check --model m18`. Populate reviewed physical records only after completing the model plan on the named unit/firmware/session.

Explicit startup-logo upload uses the descriptor screen size and persistent `LOG` storage. Ordinary profile rendering never uses that command. Restart persistence and firmware acceptance remain physical checks.

</details>

Current image/input/boot contracts and firmware restrictions are in the [revision-pinned codec matrix](../protocol-audit.md#revision-pinned-model-variant-and-firmware-matrix). All matching remains experimental; no physical results were produced by this refresh.

[All hardware modules](../README.md) · [Manager setup](../../README.md)
