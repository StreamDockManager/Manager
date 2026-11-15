# Hardware support

Choose your device, connect it to Manager, and assign your first action. All adapters are included with the app. The sixteen model modules remain **experimental**, even where a user has tested a physical unit.

[Start with Manager](#start-with-manager) · [Find your model](#find-your-model) · [Troubleshooting](#troubleshooting) · [Develop an adapter](#develop-an-adapter)

## Start with Manager

1. [Install or build Manager](../README.md).
2. Close VSD Craft and any other software using the device.
3. Connect the Stream Dock, open Manager, and select a profile.
4. Assign an action and save the binding. Use the simulator to explore layouts without hardware.

On Linux, follow the [installation guide](../packaging/install-instructions/linux/INSTALL-INSTRUCTIONS.md) to install the generated USB access rules for supported interfaces, then reconnect the device. Run as your normal desktop user. Some source-derived variants deliberately have no automatic permission grant.

## Hardware-tested devices

These models have been tested on user hardware: **N4 Pro, N1 (VSD variant), N3 (VSD variant), and K1 Pro**. This does not qualify every firmware, USB variant, operating system, or feature. Other entries below are implemented adapters, not hardware-tested claims.

## Find your model

| Model                                         | Internal ID | Current status            |
| --------------------------------------------- | ----------- | ------------------------- |
| [293](293/README.md)                          | `293`       | Experimental              |
| [293 V3](293-v3/README.md)                    | `293-v3`    | Experimental              |
| [293s](293s/README.md)                        | `293s`      | Experimental              |
| [293s V3](293s-v3/README.md)                  | `293s-v3`   | Experimental              |
| [H1 Pro](h1-pro/README.md)                    | `h1-pro`    | Experimental              |
| [K1 Pro](k1-pro/README.md)                    | `k1-pro`    | Experimental              |
| [M18](m18/README.md)                          | `m18`       | Physical support disabled |
| [M3](m3/README.md)                            | `m3`        | Experimental              |
| [Mini](mini/README.md)                        | `mini`      | Experimental              |
| [N1](n1/README.md)                            | `n1`        | Experimental              |
| [N1 (VSDinside / TreasLin)](n1-vsd/README.md) | `n1-vsd`    | Experimental              |
| [N3](n3/README.md)                            | `n3`        | Experimental              |
| [N3 (VSDinside / TreasLin)](n3-vsd/README.md) | `n3-vsd`    | Experimental              |
| [N4](n4/README.md)                            | `n4`        | Experimental              |
| [N4 Pro](n4-pro/README.md)                    | `n4-pro`    | Experimental              |
| [XL](xl/README.md)                            | `xl`        | Experimental              |

Follow the model link for USB identities, setup notes, and limitations. Similar product names can use different protocols. Disabled variants remain available for schematic previews and existing profile data; they are not enabled for physical operation.

## Troubleshooting

| Problem                          | First step                                                                                                                                                     |
| -------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Device is missing                | Close VSD Craft/other device managers, reconnect USB, and confirm the exact variant in its model guide.                                                        |
| Permission denied on Linux       | Follow the [Linux installation guide](../packaging/install-instructions/linux/INSTALL-INSTRUCTIONS.md), then reconnect the device. Do not run Manager as root. |
| Wrong layout or missing controls | Compare the model ID and USB identity. The VSD N1/N3 adapters are separate from the similarly named models.                                                    |
| Hardware is not available        | Use the simulator to prepare profiles; disabled physical variants cannot be enabled just by assigning a profile.                                               |

## Firmware checks

Device settings → Firmware reads the installed version on connection and on **Check**.
All sixteen model modules declare a source-derived read contract; this is not a claim
that every unit or firmware has been physically qualified. M18 and other disabled
variants remain unavailable for physical matching.

- 293, 293 V3, 293s, 293s V3, H1 Pro, K1 Pro, M18, M3, Mini, N1, N3, N4,
  N4 Pro and XL use HID GET_INPUT_REPORT (513 bytes). K1 Pro selects report ID 4;
  the others select 0. The SDK's common base class performs this query for every model.
- N1/N3 VSDinside variants use their independent mirajazz protocol: a 20-byte
  GET_FEATURE_REPORT with ID 1 on macOS/Linux. That source explicitly does not
  implement the Windows read, so Manager reports it unsupported there.
- Empty, malformed, mismatched-report and failed reads never become a version.
  USB release numbers are displayed separately. Disconnected or failed cached values
  are not treated as current. Simulator values are unmeasured.
- Latest-release discovery is separately supported only for exact established catalog
  mappings: N4 Pro VSDinside/MiraBox and K1 Pro `6603:1015` / EU `6603:1019`.
  Missing or ambiguous catalog mappings are shown as unavailable, not “up to date.”
  No firmware files or updater executables are downloaded or run.

Read contracts are based on the manufacturer SDK
[`82efe4d` base class](https://github.com/MiraboxSpace/StreamDock-Device-SDK/blob/82efe4d97e6bdbb9b10cbf1a80fd53d8393b278e/Python-SDK/src/StreamDock/Devices/StreamDock.py),
[transport](https://github.com/MiraboxSpace/StreamDock-Device-SDK/blob/82efe4d97e6bdbb9b10cbf1a80fd53d8393b278e/Python-SDK/src/StreamDock/Transport/LibUSBHIDAPI.py),
and the existing `fc124cb` H1 Pro/Mini records. The Python rewrite's tentative
`get_input_report` call is not copied literally: Manager uses Rust HIDAPI's buffer API,
selects the model's report ID, and removes report framing before parsing.
Retail reads follow pinned
[mirajazz `f0a7fa3`](https://github.com/4ndv/mirajazz/blob/f0a7fa3f9b5eaaa9b699c667312336d9c56f6723/src/device.rs).
Versions are recognized conservatively; unknown formats remain unknown rather than guessed.

## Inspect available models

From the **Manager repository root**, after setting up the [contributor prerequisites](../CONTRIBUTING.md):

```sh
cargo run -p stream-dock-manager-cli -- models
cargo run -p xtask -- hardware check
```

`models` lists the registered descriptors; `hardware check` validates their source files and fixtures without qualifying a physical device. `device.toml` in each module defines identity, controls, and capabilities. The simulator uses those same descriptors; its enclosure drawings are schematic.

## Develop an adapter

| File or folder           | Purpose                                                      |
| ------------------------ | ------------------------------------------------------------ |
| `device.toml`            | Stable model/control IDs, layout, variants, and capabilities |
| `src/`                   | Model-specific codec and behavior                            |
| `tests/`, `fixtures/`    | Software checks and labeled protocol evidence                |
| `verification/plan.toml` | Physical qualification procedure                             |
| `verification/results/`  | Reviewed verification records, when present                  |

To add a model, create its crate, register it in the workspace and hardware registry, then regenerate and validate:

```sh
cargo run -p xtask -- types
cargo run -p xtask -- hardware generate
cargo run -p xtask -- hardware check --model your-model-id
```

Run the model crate's tests too. Shared wire helpers belong in `crates/hardware-protocols`; model quirks stay in the model crate. Avoid model-name special cases in the frontend. Persisted control IDs require deliberate migration when changed.

Compatible variants may declare a `profile_family`. Keep exported authoring compatibility data consistent with these descriptors after hardware changes.

## Evidence and limitations

Only reviewed physical results qualify an exact variant, firmware, and platform. Source-derived fixtures, browser previews, replay results, and host transfer rates do not establish physical behavior or displayed FPS.

See the [protocol audit](protocol-audit.md), [protocol provenance](protocol-evidence.md), [source inventory](protocol-sources.v1.json), and [retail variant evidence](retail-protocol-sources.v1.json). Preserve those identities when changing a codec. Large captures belong in ignored artifact folders.
