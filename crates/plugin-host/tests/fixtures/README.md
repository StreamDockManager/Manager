# Plugin archive fixtures

Small, project-owned plugin ZIPs used to test installation and archive validation. These are test inputs, not plugins to install for normal use.

## Getting started

These archives are already checked in. You do not need to download or install a plugin. Run the Rust check below, then inspect the [package tests](../../src/packages.rs) when adding a new archive case. For executable SDK or plugin transport checks, use the separate [plugin acceptance fixtures](../../../../tests/fixtures/plugins/README.md).

## Run the checks

From the **Manager repository root**, after setting up the [contributor prerequisites](../../../../CONTRIBUTING.md):

Prepare the [pinned native dependencies](../../../../CONTRIBUTING.md#prepare-patched-rust-dependencies) before running raw Cargo commands in a fresh checkout:

```sh
python3 scripts/prepare_patched_deps.py
cargo test -p stream-dock-manager-plugin-host --lib packages
```

## What is included

| Property             | Value                                       |
| -------------------- | ------------------------------------------- |
| Sample package       | `org.vendor.counter.sdPlugin`               |
| Compression variants | Stored, Deflate, and ZIP method 14 LZMA     |
| Legacy manifest      | No plugin UUID; four-part version `0.1.0.0` |
| Source and license   | Project-owned; no third-party plugin code   |

The package tests decode each representation, preserve original manifest bytes, and exercise rejection of invalid paths, CRCs, data, LZMA properties, dictionaries, and staging operations.

## Validate a built package

Production Go-generated archives are checked separately with explicit package paths for the current host platform:

```sh
cargo run -p stream-dock-manager-plugin-host --example validate_packages -- \
  --paths /absolute/path/to/package.zip
```

You can provide more ZIP paths after `--paths`. This example checks the expected first-party package contract without installing or executing the plugin; it is not a general validator for every vendor package. Keep these fixtures independent so both sides do not share the same packaging mistake.

[Plugin acceptance fixtures](../../../../tests/fixtures/plugins/README.md) · [Contributing](../../../../CONTRIBUTING.md)
