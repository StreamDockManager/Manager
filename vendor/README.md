# Reproducible patched Rust dependencies

Manager tracks [pinned metadata](dependencies.json) and reviewed [patches](../patches/), not full third-party source trees. Make, npm desktop builds, packaging and CI prepare the crates for you.

## Use raw Cargo

From the Manager repository root, run:

```sh
python3 scripts/prepare_patched_deps.py
cargo check --locked --workspace
```

Run preparation once before opening a fresh checkout in an IDE that invokes Cargo. Cargo resolves these path dependencies before it can run any build scripts.

| Preparation mode                        | What it does                                                                                                     |
| --------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| Default                                 | Verify and reuse the prepared crates, or download and patch the pinned archives.                                 |
| `--offline` or `CARGO_NET_OFFLINE=true` | Reuse only verified local archives or patched sources. Cargo’s own registry dependencies also need to be cached. |
| `--check`                               | Verify existing prepared sources without downloading or rebuilding them.                                         |

The disposable `vendor/.cache/` is ignored by Git. Re-run preparation to recreate it. Do not edit or commit generated source trees.

## Integrity, caching and offline builds

- Archives come only from the fixed HTTPS `static.crates.io/crates/<name>/<name>-<version>.crate`
  URL. Archive SHA-256, patch SHA-256 and final source-tree SHA-256 are pinned in
  `dependencies.json`. Unexpected redirects and checksum mismatches fail closed.
- Extraction accepts bounded regular files/directories only, rejecting links,
  traversal and duplicate entries. Patches apply with exact context and offsets,
  never fuzzy matching. No external `patch` program or global Cargo-cache mutation
  is needed.
- Archives live in `vendor/.cache/archives/`; patched crates live in
  `vendor/.cache/sources/`. All of `.cache/` is ignored. Cargo overrides and the
  GLib regression-test dependency point to those generated sources.
- Every preparation validates the patch files and the entire prepared source tree.
  A verified tree is reused without changing source timestamps. A modified regular
  source tree is rebuilt from a verified archive. Unsafe links fail closed.
- A process lock serializes concurrent preparation. Each crate is fully verified
  in a staging directory before publication. Failures do not publish partial trees.
  To change a dependency or patch, review the official archive and local diff,
  update all affected hashes in `dependencies.json`, regenerate from the archive,
  and run the relevant native tests. Preserve licenses and copyright. Hashes must
  identify independently reviewed bytes, not be updated merely to accept a cache
  that failed verification.

## Patch overview

| Crate                                       | Why Manager patches it                                                 |
| ------------------------------------------- | ---------------------------------------------------------------------- |
| [GLib 0.18.5](#glib-0185-security-backport) | Variant iterator security backport and Rust syntax fixes.              |
| [Wry 0.57.3](#wry-0573-compiler-fixes)      | Linux WebView IPC lifetime and platform compiler fixes.                |
| [HIDAPI 2.6.7](#native-hid-readiness)       | Native input readiness on macOS, Windows and supported Linux backends. |

## GLib 0.18.5 security backport

The supported Tauri/GTK/WebKitGTK family requires GLib 0.18. The
[VariantStrIter patch](../patches/glib-0.18.5-variant-iter.patch) backports
[RUSTSEC-2024-0429](https://rustsec.org/advisories/RUSTSEC-2024-0429.html): the output
pointer is mutable and passed as `&mut p` to `g_variant_get_child`, preserving the
C function's write under Rust optimization.

The separate [Rust syntax patch](../patches/glib-0.18.5-rust-syntax.patch) adds
explicit lifetimes and removes unnecessary trait-object parentheses. It changes
no ownership or runtime behavior and suppresses no diagnostics. Published TOML
formatting is retained; the previously checked-in formatting-only TOML changes
are not needed for compilation.

- Published crate: [glib 0.18.5](https://crates.io/crates/glib/0.18.5), MIT.
- Archive SHA-256: `233daaf6e83ae6a12a52055f568f9d7cf4671dabb78ff9560ab6da230ce00ee5`.
- Original source commit: `42b9caf98e03ded086362d9653ca58fe94dc8658`.
- [Upstream fix PR 1343](https://github.com/gtk-rs/gtk-rs-core/pull/1343):
  `b5a4071e439bef2b5eea76c3aa25e5ae84839e34`.

This remains a reviewed backport, not a version-only advisory suppression.
Remove the override only when the supported dependency family uses a published
fixed implementation, retaining the regression. On Linux after preparation:

```sh
cargo test --locked --release -p stream-dock-manager --test glib_variant_iter
```

## Wry 0.57.3 compiler fixes

The upstream [Linux IPC fix](https://github.com/tauri-apps/wry/pull/1939)
holds a weak reference to the WebView in its IPC handler. Retain the native
close/reopen regression below when changing this dependency.

The separate [cfg patch](../patches/wry-0.57.3-check-cfg.patch) registers the existing
`macos_12_unavailable` cfg unconditionally. It neither enables the cfg nor changes
macOS detection/rendering. Other unexpected cfg names remain checked.

The [platform-warning patch](../patches/wry-0.57.3-platform-warnings.patch)
keeps the WebKitGTK 2.38 minimum: the six legacy JavaScript calls have local
compatibility annotations because their replacement requires 2.40. It removes
an undeclared, unreachable `v2_42` feature branch while retaining the existing
display-capture fallback. On Apple platforms it gates `NSObject` to the debug,
devtools and proxy configurations that use it, and marks the release-unused
error parameter without changing debug logging. No crate-wide warnings are disabled.

- Published crate: [wry 0.57.3](https://crates.io/crates/wry/0.57.3), MIT/Apache-2.0.
- Archive SHA-256: `ceb43f824178e8110e3181bfd478359c43b041452a9560d321bd8d121e468f86`.
- Published VCS metadata: `3b6eab2de073cd9da7ffeec94016db7441a0caa7` (`dirty: true`, as published).

On Linux with WebKitGTK helpers, normal build libraries and Xvfb, after preparation:

```sh
xvfb-run -a cargo test --locked -p stream-dock-manager --test wry_ipc_lifetime -- --ignored --test-threads=1
```

The test verifies real IPC before close, release of captured state, and reopening
with the same retained context. Keep these narrowly scoped patches until the
supported upstream release contains the fixes.

## Native HID readiness

`hidapi` 2.6.7 uses its crates.io archive checksum plus reviewed Apple, Windows,
and Linux patches in `patches/`. All patches and the resulting tree are hashed in
`dependencies.json`. The additive safe Rust callback API owns callback storage,
contains unwinds, and synchronously retires observers before replacement or close.
Callbacks may only signal independent synchronization state, never reenter HIDAPI
or acquire a lock held across a HIDAPI call. Notifications never consume reports.

- Apple signals under the existing input queue mutex on report enqueue,
  disconnection and reader shutdown, including already queued/closed registration.
- Windows observes the existing OVERLAPPED read's manual-reset completion event.
  One cancellable thread notifies once then waits for the next read's explicit
  rearm. It never opens a device or consumes/resets a completion. The original
  reader resets the event before arming each read. Close retires the observer and
  cancels/completes a pending read before releasing its buffer and OVERLAPPED.
- Bundled Linux static-hidraw observes the existing fd through edge-triggered
  epoll with eventfd cancellation. It does not reopen, duplicate or read the fd.
  Other Linux backends and the alternate Windows-native backend return an
  unsupported error, retaining the caller's bounded fallback.

Runtime attempts supported readiness by default. Setup failure retains bounded
polling. Terminal observer failure wakes the reader and yields a read error so
normal connection retirement/reconnect handles it; it cannot silently park forever.
If an OS cancellation/join unexpectedly fails, callback/device storage remains
alive rather than risking use-after-free.

`python3 scripts/test_hidapi_readiness.py` compiles the patched Apple C backend,
checks its symbol, and exercises a synthetic queue and callback retirement.
`python3 scripts/test_hidapi_windows_readiness.py` builds the actual Windows backend
and synthetic event fixture using an installed GCC toolchain, and runs the fixture
only on Windows. Cross-compilation is not native execution. These fixtures never
open hardware and do not qualify device report latency, firmware transactions,
unplug/reconnect, or suspend/resume. Native hardware qualification is still pending.
