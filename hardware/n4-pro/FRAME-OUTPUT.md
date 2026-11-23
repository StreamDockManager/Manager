# N4 Pro rendering methods

For normal setup, use the [N4 Pro guide](README.md) and [Manager getting started](../../README.md#set-up-your-first-control).
This page explains the rendering policy for adapter contributors and troubleshooting.

| Method             | When it is used           | What it does not promise                                |
| ------------------ | ------------------------- | ------------------------------------------------------- |
| Frame scheduling   | Default                   | A guaranteed physical frame rate or atomic presentation |
| Individual updates | Explicitly saved fallback | Identical batching behavior to frame scheduling         |

Frame scheduling is the default;
Individual updates is an explicitly saved fallback. These are host policies and do
not qualify physical FPS or atomic presentation.

The N4 Pro codec exposes an explicit `n4-pro-key-stp-v1` commit for the ten LCD
keys. Complete BAT uploads may share one final STP at a legal group boundary.
Touch image slots 1–4 retain their individual STP; background BGPIC and ordinary
commands split groups. An active group completes before scene invalidation or
ordinary commands; physical disconnect/write failure can stop immediately.

[The vendor audit](../protocol-audit.md) records the pinned source, transport
symbols, report padding, BGPIC field order and unresolved input differences.
Historical captures and qualification records retain their original scope.
Synthetic fixtures and simulator tests do not certify the other variants.
