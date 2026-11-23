# Hardware support

All 16 adapters are built into Manager. Use the model guides to match your USB ID, check what works, and assign your first action. Physical support is **experimental**, even for models tested on real devices.

[Get started](#start-with-manager) · [Model guides](#find-your-model) · [Troubleshooting](#troubleshooting) · [Firmware](#firmware-checks) · [Contribute](#develop-an-adapter)

## Start with Manager

1. [Install Manager](../README.md#install) and close other dock software, such as VSD Craft.
2. Connect your dock. Open Manager and select the device and a profile in **Device editor**.
3. Select a control, choose an action, and save the binding.

- **No hardware?** Explore device layouts and build profiles in the simulator.
- **Linux:** Set up [USB access](../packaging/install-instructions/linux/INSTALL-INSTRUCTIONS.md), then reconnect. Run as your normal desktop user. Some experimental variants have no automatic access rule.

## Hardware-tested devices

[N4 Pro](n4-pro/README.md), [N1 (VSD variant)](n1-vsd/README.md), [N3 (VSD variant)](n3-vsd/README.md), and [K1 Pro](k1-pro/README.md) have been tested on user hardware. Those results do not cover every variant, firmware, OS, or feature.

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

Follow a model link for USB IDs, setup, and limitations. Match the USB ID, not just the product name. Disabled variants remain available for schematic previews and existing profiles but cannot connect to a physical device.

## Troubleshooting

| Problem                          | First step                                                                                                                                                     |
| -------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Device is missing                | Close VSD Craft/other device managers, reconnect USB, and confirm the exact variant in its model guide.                                                        |
| Permission denied on Linux       | Follow the [Linux installation guide](../packaging/install-instructions/linux/INSTALL-INSTRUCTIONS.md), then reconnect the device. Do not run Manager as root. |
| Wrong layout or missing controls | Compare the model ID and USB identity. The VSD N1/N3 adapters are separate from the similarly named models.                                                    |
| Hardware is not available        | Use the simulator to prepare profiles; disabled physical variants cannot be enabled just by assigning a profile.                                               |

## Firmware checks

Go to **Device settings → Firmware** and choose **Check**. An unreadable or unsupported version does not mean your device is up to date. Manager can look for a newer release only when it has a verified catalog mapping for the exact variant. **Instructions** opens the vendor guide; Manager does not download firmware or run an updater.

<details>
<summary>Read protocols, supported catalog mappings and pinned sources</summary>

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

</details>

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

### Firmware instruction links

Every model explicitly declares top-level `firmware_instructions` and
`firmware_instructions_shared = true` before the first TOML table. Shared guidance
must remain labeled shared even when its URL is copied into a model manifest.
A verified model-specific guide may set the flag to false.

Each `[[variants]]` entry in a model's `hardware/<model>/device.toml` may define
`firmware_instructions = "https://…"`. Put it beside `id` and `name`, before
`[variants.identity]`. A variant using general brand guidance must also set
`firmware_instructions_shared = true`; exact verified variant guides default to false. Omission uses the model-level guide, then the canonical shared N4 Pro VSD guide if a future model omits both.
This is general guidance, not proof of firmware compatibility. Unknown models, unknown
variants, disabled variants, and simulators receive no fallback. The connected exact variant supplies
both the Firmware section's availability and the native Instructions action.

Only reviewed tutorial slugs under `https://www.vsdinside.com/blogs/tutorial/` or
`https://mirabox.net/blogs/tutorial/` are accepted. HTTP, credentials, ports, queries,
fragments, encoded paths, and firmware artifacts are rejected. Review the destination as
actual firmware guidance for that variant before editing the field; a generic software
update page is not a device firmware guide. A new vendor origin requires a code-policy
review, not merely another manifest URL. Exact links currently cover VSDInside N4 Pro and MiraBox N4 Pro. The explicitly branded MiraBox N4 Pro E uses the shared MiraBox software-interface guide.
Other variants without verified brand-specific guidance, including every K1 Pro variant,
use the shared VSD N4 guide. Shared SDK provenance or a USB vendor ID alone does not
establish a retail brand. That fallback URL
is read from `hardware/n4-pro/device.toml`, variant `vsdinside`, as a defensive fallback, never from Rust constants. Resolution order is variant, model, then fallback; catalog URLs never participate.

After an edit, run `cargo run -p xtask -- types`, `cargo run -p xtask -- hardware check`,
and the `firmware_instructions` hardware-core and `instructions_follow_hardware_source`
runtime tests. The native action opens documentation only, and never performs suggested
vendor support, updater, or device actions automatically.
