# K1 Pro

This adapter is built into [Stream Dock Manager](../../README.md).

| At a glance             | Details                                                               |
| ----------------------- | --------------------------------------------------------------------- |
| Model ID                | `k1-pro`                                                              |
| Adapter status          | Experimental                                                          |
| Controls                | 6 buttons, 3 encoders                                                 |
| Hardware testing        | Tested on a user unit; results do not cover every variant or platform |
| Layout and capabilities | [Device descriptor](device.toml)                                      |

Match the [USB ID](#compatibility), not just the product name. Dials are called encoders in the descriptor.

[Get started](#connect-and-assign-an-action) · [Keyboard settings](#keyboard-settings) · [Sleep and wake](#sleep-and-wake) · [Compatibility](#compatibility) · [Verification](#verification)

## Connect and assign an action

1. [Install Manager](../../README.md) and close other dock software, such as VSD Craft.
2. Connect your dock. Open Manager and select the device and a profile in **Device editor**.
3. Select a control, choose an action, and save the binding.

- **No hardware?** Select this model in the simulator.
- **Linux:** Follow the [USB access setup](../../packaging/install-instructions/linux/INSTALL-INSTRUCTIONS.md), then reconnect. Run as your normal desktop user and do not broaden HID permissions.

## Navigation page

Each K1 Pro profile includes a protected **Navigation** page. With no profile loaded, the keyboard shows Navigation. If the current page stays blank for 30 seconds, Manager returns to Navigation.

| Row    | Default keys             |
| ------ | ------------------------ |
| Top    | Insert · Home · Page Up  |
| Bottom | Delete · End · Page Down |

Use **Appearance** to change labels, fonts, icons, colors or images. **Reset** restores the theme-driven appearance. The page and its six actions cannot be removed or reassigned; the dials and other pages remain editable.

Manager needs permission to send keyboard shortcuts. macOS maps Insert to Help/Insert; the focused app decides whether to handle it.

## Keyboard settings

Open **Device settings → Keyboard**.

| Control                                  | What it does                                                                                                                                                                           |
| ---------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Effect**, **Speed**, **Custom colors** | Choose from 16 effects, set speed from 0–5, or switch between a custom color and the effect’s own palette. **Ripple (background)** also lets you pick a background hue and saturation. |
| **Apply**                                | Save changes. OS layout and sleep timer changes are applied only here; changing OS layout may reconnect the keyboard.                                                                  |
| **Cancel**                               | Revert the lighting preview. Escape or closing the editor does the same.                                                                                                               |
| **Reapply saved settings**               | Send saved preferences again. Manager does not automatically replay them on connect.                                                                                                   |
| **Factory Default**                      | After confirmation, reset on-device assignments, records and images. Manager profiles and bindings remain intact.                                                                      |

Lighting previews require a readable device baseline so Manager can restore what was there before. If your firmware does not provide enough settings, use **Apply** instead. Preview expires after five idle minutes and ends on disconnect or a conflicting change.

The Keyboard tab prefers the device’s reported settings. Fields this firmware does not report stay unknown, including after a reset. Manager does not guess a value or retry the reset automatically. Host completion does not qualify device persistence or readback.

## Sleep and wake

**Sleep after** in the Keyboard tab sets the device's idle timer to **1, 5, 15 or
30 minutes, 1 or 2 hours, or Never**, matching VSD Craft's timed presets. Select a
value and choose **Apply**. Opening the controls, changing the draft, live lighting
preview, or Cancel does not send the timer. **Not set** means Manager has no saved
request; it is not a hardware value. This is a firmware timer request, not a forced
sleep or a promise that the LCD backlight powers off. Follow-system behavior is
separate from a device timer and is not offered here. Timer timing and wake behavior
remain unqualified on physical hardware.

Sleep requests a software-managed blackout: display brightness zero, actual black
JPEG frames on all six LCD keys, and keyboard brightness zero when a saved
keyboard level is available. It does not send `HAN`, whose behavior varies by
firmware. Wake restores the current LCD scene and animations, plus the captured
keyboard brightness, effect, speed and custom color in encoder order. Preferences
are unchanged, and keyboard OS mode is never replayed.

Unknown keyboard brightness is left unchanged with a warning: apply a level once
before using reversible blackout. Unknown lighting fields are not guessed. This
is a visual blackout, not USB suspend or confirmed panel power-off. Host completion
is not hardware verification. Interrupted or failed transfers are reported and
are not automatically retried. A disconnect loses the temporary restoration
token; reapply keyboard settings after reconnect if necessary. Shutdown retains
the SDK's `HAN` only rather than creating an override that cannot be restored.

## Compatibility

| Variant            | USB vendor:product | Physical matching |
| ------------------ | ------------------ | ----------------- |
| K1 Pro (6603:1015) | `6603:1015`        | Enabled           |
| K1 Pro (5548:1025) | `5548:1025`        | Enabled           |
| K1 Pro (6603:1019) | `6603:1019`        | Enabled           |

All variants remain **experimental**. The adapter follows the pinned Python SDK revision with the differences documented below. Firmware, platform and variant behavior still need physical verification. Ordinary keyboard key
remapping and background/startup-logo uploads are not exposed. The schematic
places three dials over two rows of LCD keys; geometry is not a physical measurement.

<details>
<summary>Developer reference: protocol and verification</summary>

## Protocol reference

Input/image mapping and most command payloads follow the explicitly selected
[Python SDK revision 82efe4d](https://github.com/MiraboxSpace/StreamDock-Device-SDK/tree/82efe4d97e6bdbb9b10cbf1a80fd53d8393b278e),
particularly [K1Pro.py](https://github.com/MiraboxSpace/StreamDock-Device-SDK/blob/82efe4d97e6bdbb9b10cbf1a80fd53d8393b278e/Python-SDK/src/StreamDock/Devices/K1Pro.py)
and [LibUSBHIDAPI.py](https://github.com/MiraboxSpace/StreamDock-Device-SDK/blob/82efe4d97e6bdbb9b10cbf1a80fd53d8393b278e/Python-SDK/src/StreamDock/Transport/LibUSBHIDAPI.py).
Exact hashes and historical comparisons are in [the source inventory](protocol-sources.v1.json).

- LCD logical keys 1–6 map to **5, 3, 1, 6, 4, 2**, for both image addressing and input.
  Stable logical control IDs and saved bindings are unchanged.
- Images are 64×64 JPEG, clockwise 90°. Upload headers are `BAT`, big-endian
  32-bit image length and hardware key. Image chunks retain 1023 payload bytes plus a trailing pad.
  Manager retains explicit `STP` refreshes after complete uploads/grouped frames.
- Input framing remains numbered report 4, `ACK` and `OK`. State 1 means pressed;
  other states mean released, matching this Python decoder. Rotation ignores the
  state byte. Encoder presses remain pulses without a claimed release capability.
- Every command body starts `CRT 00 00`. Display brightness is `LIG 00 00 value`;
  keyboard brightness is `LLUM 00 percent` with a raw 0–100 percentage, matching
  the official web app’s continuous slider (not the Python SDK’s older 0–6 range).
  Existing saved values retain their exact transmitted percentage; they are not
  silently scaled. Custom color uses the official web app’s
  `COLOR 01 r g b hue_hi hue_lo saturation value` payload, with HSV derived from RGB.
  It is sent after effect/speed and before OS mode in a combined Apply.
- Timed sleep uses `SLPT 00` followed by a four-byte unsigned big-endian number
  of seconds; `ff ff ff ff` selects Never. It is not part of lighting preview,
  blackout restoration, or connection replay. The official web transport defines
  the payload; the installed VSD Craft resources provide the selected presets.
- Effects use `LMOD 00 effect`; speed uses **`LSPD 00 speed`**, with supported levels 0–5. Selecting Static
  first sends `LSPD 00 00`, then `LMOD 00 00`. An explicit speed follows the effect.
  Windows/macOS mode is `CPOS 00 W/M`, issued last.
- All 16 named effects follow the official web app's transmitted list indexes:
  0 Static, 1 Gradient, 2 Breathing, 3 Wave, 4 Ripple, 5 Windmill, 6 Fountain,
  7 Fireworks, 8 Band, 9 Pixel, 10 Flash, 11 Halo, 12 Rush, 13 Grid,
  14 Ripple (background), and 15 Game. Decorative vendor item IDs are not wire IDs.
- The second Ripple (index 14) supports a separate background hue/saturation:
  `BASECO hue_hi hue_lo floor(saturation_percent / 100 * 255)`. The command has
  no RGB brightness/value byte. Background is sent after the foreground COLOR
  and before OS mode. Stored background hue is 0–359 and saturation is 0–100;
  it is retained when switching effects but only applied with a supported effect.
  No hardware default background is assumed.
- Explicit **Factory Default** follows the official web app's layered K1 reset:
  `LYRST FF`, then `REREC`. It resets on-device key/layer assignments and records/images,
  not just lighting. Manager profiles are separate and must not be deleted by this command.
  Reset does not imply known numeric defaults; query the device afterward.
- Read-only settings query is `INFO 00`. A `DEVC` input reply can report effect, speed,
  OS mode and keyboard brightness in its binary form, or those fields plus custom-color
  flag, HSV, background hue/saturation and sleep timer in JSON. Missing fields remain
  unknown. HSV is converted to RGB at the reported byte precision. A reported zero sleep
  timer has no verified interpretation and is not substituted with a guessed default.
  These are source-derived formats, not physical reset/readback qualification.
- Initialization is wake `DIS`, clear `CLE 00 00 FF`, then refresh `STP`.
  Sleep is `HAN`; heartbeat is `CONNECT`.

### Deliberate transport safety differences

HIDAPI requires a report-ID byte. Manager supplies `04` followed by a 1024-byte
body (1025-byte API buffer), checks short writes and serializes transactions.
The Python transport stores report ID 4 but omits it from writes; blindly copying
that omission would make leading ASCII `C` the HIDAPI report ID. Its conditional
leading-zero bulk workaround is not copied: every Manager image report has an
explicit report ID, independent of its first pixel/compressed byte. These are
API-buffer claims, not verified USB captures.

The image stream also retains the existing 1023-byte chunks plus trailing pad,
instead of copying Python's 1024-byte chunks. [DeckBridge's K1 notes](https://lukasmega.github.io/DeckBridge/devices/#mirabox-k1pro)
report firmware dropping the final byte of full chunks and hardware verification
of this padding workaround; [SDK issue #87](https://github.com/MiraboxSpace/StreamDock-Device-SDK/issues/87)
reports image-dependent corruption in both vendor examples. This is independent
reported evidence, not our physical qualification. Changing this framing merely
to mirror Python would risk introducing corruption.

### Conflicting vendor sources

The earlier native transport uses `LMOD` for both effect and speed and packs RGB
without the three zero bytes. The official web app uses `LSPD` and different
color fields/ranges. Manager now uses the official web `sendSpeed` command (`LSPD`, speed 0–5), matching its discrete slider. This supersedes Python `LSPE` 0–7 after the user reported that speed never changed. This does
not establish that every firmware accepts it.

RGB is an explicit exception after the Python payload failed to change color on
the user’s unit. The [official web transport](https://app.mirabox.net/assets/index-DmfhZ_Gy.js)
uses a custom-color flag and RGB/HSV fields. Its [lighting UI](https://app.mirabox.net/assets/pressKey-ms0tZ_QU.js)
enables the color picker with flag 1 and disables it with flag 0, retaining the
selected RGB/HSV in both commands. Manager follows that toggle.
Python’s three zero bytes instead disable this flag and place RGB at different
offsets. Manager now sets custom-color flag 1, RGB bytes, a big-endian integer
hue, and byte-scaled saturation/value. It does not silently switch the selected
effect to Static. Some effects may ignore custom color; the vendor UI disables
custom color for effect 1. The corrected RGB payload remains source-derived,
not physically verified. Exact source hashes are in the inventory.
Do not label synthetic fixtures physical evidence.

## Verification

From the Manager repository root:

```sh
python3 scripts/prepare_patched_deps.py
cargo test -p stream-dock-manager-hardware-k1-pro
cargo run -p xtask -- hardware check --model k1-pro
```

Tests cover logical key addresses, image reconstruction/chunk boundaries, all
input states, keyboard parameter ranges and payload bytes, all 16 effect indexes, background hue/saturation, static ordering,
initialization and display brightness. Fixtures are synthetic, never captured HID.
Follow [the physical plan](verification/plan.toml) on the exact unit, firmware and
platform to verify visible key positions, brightness/color/effects/speed, OS mode,
reconnect and persistence. No unit/firmware/platform is fully qualified by this change.

Keyboard brightness previews send only the changed luminance instead of restarting
mode, speed and color on every slider update. Compound lighting writes apply `LLUM`
after effect/color/background commands (and before an optional OS-mode change), so
those commands cannot immediately replace the requested luminance. Cancel and wake
restoration use the same ordering. This ordering is regression-tested with captured
reports; visible brightness retention still requires physical-device verification.

</details>

[All hardware modules](../README.md) · [Manager setup](../../README.md)
