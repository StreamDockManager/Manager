# Reproducible patched Rust dependencies

Manager keeps only [pinned dependency metadata](dependencies.json) and reviewed
patches in Git. Full third-party source trees are downloaded and prepared locally,
not checked in. Supported Make, npm desktop/check, packaging and CI entry points
run preparation before Cargo resolves its path dependencies.

Before using **raw Cargo** from a fresh checkout, run:

```sh
python3 scripts/prepare_patched_deps.py
cargo check --locked --workspace
```

Cargo must resolve dependency manifests before it can run any `build.rs`, so a
Cargo build script cannot bootstrap missing path dependencies. The explicit step
is required for direct Cargo commands and IDEs on a fresh checkout.

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
- `python3 scripts/prepare_patched_deps.py --offline` (also selected by
  `CARGO_NET_OFFLINE=true`) never downloads. A verified source tree or cached archive
  is required; a missing/corrupt offline cache is an actionable error, not an
  unpatched registry fallback. Once both crates are prepared, Cargo's own registry
  dependencies must also be cached for `cargo --offline` to work.
- `python3 scripts/prepare_patched_deps.py --check` only verifies prepared sources;
  it neither downloads nor rebuilds them. The cache is disposable: preparation
  recreates it. Do not edit generated sources or commit them.

To change a dependency or patch, review the official archive and local diff,
update all affected hashes in `dependencies.json`, regenerate from the archive,
and run the relevant native tests. Preserve licenses and copyright. Hashes must
identify independently reviewed bytes, not be updated merely to accept a cache
that failed verification.

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

## Wry 0.57.2 lifetime and compiler fixes

The [weak IPC reference patch](../patches/wry-0.57.2-ipc-weak-reference.patch)
breaks the Linux ownership cycle in [Wry issue 1871](https://github.com/tauri-apps/wry/issues/1871):
a WebView owns its user-content manager, so its IPC callback must not hold a strong
reference back to the view. It upgrades a weak reference only for a live message.
No helpers are killed and active plugin views remain independent. Tauri's shared
WebKit context/network helper is still intentionally retained for reuse.

The separate [cfg patch](../patches/wry-0.57.2-check-cfg.patch) registers the existing
`macos_12_unavailable` cfg unconditionally. It neither enables the cfg nor changes
macOS detection/rendering. Other unexpected cfg names remain checked.

The [platform-warning patch](../patches/wry-0.57.2-platform-warnings.patch)
keeps the WebKitGTK 2.38 minimum: the six legacy JavaScript calls have local
compatibility annotations because their replacement requires 2.40. It removes
an undeclared, unreachable `v2_42` feature branch while retaining the existing
display-capture fallback. On Apple platforms it gates `NSObject` to the debug,
devtools and proxy configurations that use it, and marks the release-unused
error parameter without changing debug logging. No crate-wide warnings are disabled.

- Published crate: [wry 0.57.2](https://crates.io/crates/wry/0.57.2), MIT/Apache-2.0.
- Archive SHA-256: `a490119196d02a51d0871df7b9119a1f3e46d75b7e90053a801454d5c6b3491c`.
- Published VCS metadata: `bbba6b6312af92fbb969789a63768f4b4bb6d268` (`dirty: true`, as published).

On Linux with WebKitGTK helpers, normal build libraries and Xvfb, after preparation:

```sh
xvfb-run -a cargo test --locked -p stream-dock-manager --test wry_ipc_lifetime -- --ignored --test-threads=1
```

The test verifies real IPC before close, release of captured state, and reopening
with the same retained context. Keep these narrowly scoped patches until the
supported upstream release contains the fixes.
