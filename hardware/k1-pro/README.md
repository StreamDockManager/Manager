# K1 Pro

Use this device with [Stream Dock Manager](../../README.md). Its adapter is included with the app; **no separate adapter download is needed**.

| At a glance                   | Details                                                                           |
| ----------------------------- | --------------------------------------------------------------------------------- |
| Model ID                      | `k1-pro`                                                                          |
| Adapter status                | Experimental                                                                      |
| Registered controls           | 6 buttons, 3 encoders                                                             |
| Hardware testing              | Hardware tested on a user unit; full variant/platform qualification remains open. |
| Exact layout and capabilities | [Device descriptor](device.toml)                                                  |

Registered controls describe the Manager model, not a promise that every control or firmware has been physically verified. Dials are called encoders in the descriptor. Match your USB variant below before relying on a retail product name.

[Get started](#get-started) · [Keyboard settings](#keyboard-settings) · [Sleep and wake](#sleep-and-wake) · [Compatibility](#compatibility) · [Verification](#verification)

## Get started

1. [Install or build Manager](../../README.md).
2. Quit VSD Craft and other software that may be using the device.
3. Connect the device, open Manager, and select it.
4. Choose a profile, select a control that supports actions, assign an action, and save the binding.

You can explore the layout in the simulator without a connected device. On Linux, follow the [installation instructions](../../packaging/install-instructions/linux/INSTALL-INSTRUCTIONS.md) and reconnect after installing applicable USB access rules. Run Manager as your normal desktop user.

## Navigation page

Every K1 Pro profile starts with a protected **Navigation** page. Its top row is
**Insert · Home · Page Up** and its bottom row is **Delete · End · Page Down**.
The six keys start with centered labels and shaded keycap backgrounds generated
from the selected theme palette. Use **Appearance** to customize their labels,
fonts, icons, colors or images; **Reset** restores the theme-driven defaults.
The page and key actions cannot be removed or reassigned. Dials and other pages
remain editable.
Existing pages and the selected page are preserved when this page is added.

These are host keyboard shortcuts, so Manager needs the normal keyboard-control
permissions. On macOS, Insert uses the platform's Help/Insert key; the focused
application decides whether it supports that key.

## Keyboard settings

Open **Device settings → Keyboard**. Lighting changes preview on the connected
keyboard; **Apply** saves them and **Cancel**, Escape or closing restores the saved
lighting. Opening the modal sends nothing. All 16 vendor effect names are available;
the second **Ripple (background)** adds a full-brightness hue/saturation background
picker. Speed supports levels 0–5. **Custom colors** switches between the selected color
and the effect’s built-in rainbow palette without discarding your chosen colors.

Manager requests keyboard settings from supported K1 units on connection and after
Apply or Factory Default. The Keyboard tab prefers the device's reported values;
fields absent from that firmware's reply stay unknown. Older binary replies report
only effect, speed, OS mode and brightness. Saved preferences remain separately
available through **Reapply saved settings** and are never silently replayed on connect.

Live preview requires a known effect, speed and color/custom-mode baseline, plus
brightness/background when previewing those fields, so Cancel can restore it safely.
If the connected device does not report enough values, use Apply rather than guessing
a rollback state. Preview expires after five idle minutes; disconnects or conflicting
changes invalidate it. OS layout and sleep timers are Apply-only. Auto selects Mac on
macOS and Windows on Linux/Windows; a layout change may re-enumerate the keyboard.

**Factory Default**, next to Reapply saved settings, asks for confirmation and sends
the vendor reset immediately. It may clear on-device assignments, records and images;
Manager's profiles and bindings remain intact. The UI refreshes from the reply, not
hardcoded factory values. No response means unknown settings, not a failed reset or
permission to retry automatically. A completed host transfer does not prove firmware
persistence or visible hardware behavior. Physical reset/readback remains unqualified.

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

All variants remain **experimental**. The November 8 correction follows the
user-selected Python SDK revision, not a physical qualification. Firmware,
platform and variant behavior must still be verified. Ordinary keyboard key
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
