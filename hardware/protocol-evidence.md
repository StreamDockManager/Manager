# Expansion protocol evidence

Use this reference when reviewing or changing an adapter. To install Manager and connect
a device, follow the [hardware README](README.md) instead. Pinned source evidence and
synthetic checks establish implementation behavior, not physical-device qualification.

[Model setup](README.md#find-your-model) · [Audit decisions](protocol-audit.md) · [Contributing](../CONTRIBUTING.md)

The additional adapters are independent Rust implementations of facts in MiraboxSpace's
[Device SDK at `fc124cb`](https://github.com/MiraboxSpace/StreamDock-Device-SDK/tree/fc124cb5bc8fe0cd6c72d3f2af80588adc7dcffb).
Exact Git blob and SHA-256 identities are in [the source inventory](protocol-sources.v1.json).
The [current revision-pinned matrix](protocol-audit.md#revision-pinned-model-variant-and-firmware-matrix) records the codec refresh, firmware gates and retained independent retail evidence; historical shared framing remains unchanged.
Reference bytes were checked against their Git blob hashes. The vendor repository uses the
[MIT license](MiraboxSpace-LICENSE.txt). No vendor transport binary, firmware, application,
image assets or executable vendor source is included in these adapters.

The [product table](https://github.com/MiraboxSpace/StreamDock-Device-SDK/blob/fc124cb5bc8fe0cd6c72d3f2af80588adc7dcffb/Python-SDK/src/StreamDock/ProductIDs.py)
resolves vendor/product tuples to concrete model classes. The model `decode_input_event`,
`set_key_image`, `set_device` and image-format methods establish input codes, output slots,
report IDs/sizes, transforms and special behavior. Read the actual method: N1 input codes
30/31 map to auxiliary buttons 16/17, while **output** uses slots 16/17 and separate display 18.
Its `KEY_COUNT` alone does not establish the control set. Likewise N3 and M18 counts cannot
be inferred from their model names or a previous simulator grid.

## Discovery

The vendor's pinned `LibUSBHIDAPI.enumerate_devices` selects exact vendor/product, usage 1,
and usage page **greater than 1025**, without an interface-number constraint. The new generic
`HidSelector` expresses that predicate; `HidIdentity` continues to represent actual observations.
Selectors overlap-check across exact/range pages and optional interfaces. Keyboard/consumer
usage pages and a different product/usage do not match. No exact interface number is invented.

Because that predicate does not establish an interface-specific Linux permission rule, these
new variants receive **no broad automatic udev grant**. Interface/permission qualification is a
recorded dependency. N4 Pro retains its existing exact interface/page/usage and narrow rule.
The old `5548:1004` N4 and `5548:1002` N1 candidates are absent from both registration tables.
`5548:1000` M18 is now present in both Python and the active C++ factory; its firmware-mode
reconciliation remains open. These entries are no longer active discovery hints and receive no
permission grant. See the [current audit](protocol-audit.md). The unused `USB_VIDN3V2` constant is not a product registration: the actual
product tuple registers `6602:2929`, as the source table does.

## Framing

Read-only inspection of the pinned public Linux transport's unstripped ELF symbols/constants
and instructions corroborates the family framing. Its exact byte identity and inspection method
are in the inventory. The library was never executed and is not redistributed. The relevant
methods are `TransportSession::chunkSize` (`0x16cd0`), `transact` (`0x19cb0`),
`TransportByteBuffer::prepend` (`0x15770`), `resize` (`0x15270`), and
`Transport::setKeyBitmap` (`0x1af80`). Symbol/constant/disassembly outputs remain in ignored
`artifacts/hardware-expansion/vendor-7f0d47f/`.

HIDAPI writes contain one report-ID byte followed by `CRT\0\0` and the command. Every report
is zero-padded to its model's declared 513 or 1025 bytes, including ID. Unnumbered transfers
carry 512/1024 payload bytes. For ID 4, the current transport's `chunkSize` subtracts one and
`setKeyBitmap` subtracts another: chunks contain **1023 bytes**, then the ID/payload and trailing
zero pad occupy a 1025-byte report. This is different from an earlier community transport's
chunk arithmetic. The implementation follows the pinned current vendor method, with explicit
boundary tests rather than transferring an N4 assumption.

K1 Pro now uses `CLE\0\0\xff` from the selected Python 82efe4d source,
while retaining the 1023-byte image chunks and sacrificial trailing pad.
Its [model audit](k1-pro/README.md#protocol-reference) explains retained HIDAPI
report IDs and other deliberate safety differences. Other models keep this baseline.

Commands are `DIS` (wake), `HAN` (sleep), `CONNECT` (10-second heartbeat),
`CLE\0\0\0\xff` (clear), and `LIG\0\0<0..100>` (brightness). `BAT` takes a big-endian
32-bit image length and one-byte model-owned slot, followed by image chunks; `STP` refreshes
keys. Persistent startup logos use `LOG`, big-endian length and flag 1, followed by chunks.
The current `setBackgroundBitmap` (`0x1b420`) and `setBackgroundImgStream` (`0x1b900`)
confirm `LOG` for persistent startup images; it must not be used for ordinary profile backgrounds.
Initialization wakes, selects an explicitly documented model mode when required, clears and
refreshes. Runtime restoration applies the saved manual brightness; the codec does not force 100%.

`MOD\0\0<mode + '1'>` follows `Transport::changeMode` (`0x1d2d0`). N1's `open()` selects
mode 2; H1 Pro's public mode enum identifies key mode 1. Optional keyboard remapping, calculator
skins, device-storage animation uploads, calibration and undocumented keyboard lighting effects
are not inferred from the common transport API.

K1 Pro has separate experimental keyboard output commands, documented with exact
native binary identities in its [source inventory](k1-pro/protocol-sources.v1.json):
`LLUM` brightness, `COLOR` global RGB, and `CPOS` explicit Windows/macOS mode.
They are emitted only by an explicit keyboard settings request, never by generic
lighting or profile restoration. Both inspected native transports encode keyboard
effect and speed using the same `LMOD` command, so neither control is exposed.
Binary-derived fixtures do not establish physical acceptance or persistence.

The current `BGPIC` method differs from earlier descriptions: its raw fields are length,
x, y, width, height, zero, layer. The pinned C++ constructors advertise live background
support only for N4 Pro, M3, and XL. Those adapters use full-frame `BGPIC`; other models
with startup-image evidence expose explicit boot-logo uploads only. Legacy saved background
assets remain readable without causing ROM writes.
Generic adapters reject partial rectangles. **N4 Pro's existing model-owned BGPIC/trajectory codec is not
rewritten by this expansion**; its existing tests remain regression requirements.

Inputs require the source's `ACK`/`OK` markers and code/state offsets. K1 Pro requires report
ID 4; unnumbered reports allow HIDAPI's optional leading zero. Truncated input is rejected,
unknown packets/codes/states are ignored. Buttons retain independent press/release edges.
Encoder presses remain pulses with no claimed release capability. Switch left/center/right
positions have distinct grouped button IDs; release ownership and scheduling stay per position.
N4 swipe labels come from its own model decoder, without borrowing N4 Pro's physical trajectory
observations. All such source-derived mappings still require physical review.

## Per-model contracts

All preview enclosures are schematic. Image dimensions below are logical source dimensions;
90-degree transforms swap the wire width/height. Vendor Pillow positive rotation is
counterclockwise, so the Rust codec records the equivalent clockwise rotation explicitly.
PNG wire images are RGBA; JPEG/raw RGB output composites transparency against black.
The raw BGR and RGB565 modes are internal wire formats, not user-imported asset formats.

The current per-model image, input, boot-logo, variant and firmware contracts are
recorded in the [revision-pinned audit matrix](protocol-audit.md#revision-pinned-model-variant-and-firmware-matrix).
The older matrix has been superseded: official N1 display-only pixels remain 80² (retail N1-VSD uses 64²); 293s secondary
surfaces are not inputs; N3 vendor 6602 selects 60² images and raw RGB565 boot output.
Boot storage is distinct from live background support. K1 Pro's upstream boot
format is recorded but its existing unavailable capability gate is unchanged, as are
N1 and H1 Pro boot gates. H1 Pro image output stays fail-closed for unknown firmware.

## Saved schematic profiles and qualification

Existing stable control IDs keep their intended cell/role. Former N3 `key-10`…`key-18`, N1
`key-18`, and N4 `touch-1`…`touch-4` are retained separately as retired control metadata.
They remain readable for persisted profiles/history and explicit conversion review. They never
become a different physical input, SDK context or output. A same-model reviewed profile copy
can deliberately map or omit them; no destination is guessed for a retired ID. Original
profiles, assignments and rules remain untouched. N1's output slot 18 is a separate display-only surface; the source does not establish a knob/display association. It never reinterprets the retired button ID.

Each module owns a descriptor, codec table, synthetic fixture and verification plan. The fixtures
are reconstructed from the pinned sources and **are not HID captures**. Tests cannot certify a
variant, firmware, native session, packet delivery, brightness/lighting appearance, frame rate,
swipe direction, asleep wake reporting or long-duration stability. Every new adapter remains
experimental with empty physical result directories. Available N4 Pro historical observations
retain their exact source/unit/session limits and do not qualify this expansion.
