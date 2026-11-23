# Plugin archive fixtures

Small, project-owned plugin ZIPs used to test installation and archive validation. These are test inputs, not plugins to install for normal use.

## Run the checks

The ZIPs are checked in. No plugin install or download is needed. From the **Manager repository root**, with the [build prerequisites](../../../../CONTRIBUTING.md):

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

The [package tests](../../src/packages.rs) check decoding and keep the original manifest bytes. They also reject invalid paths, CRCs, archive data, LZMA parameters and unsafe staging operations.

## Validate a built package

Production Go-generated archives are checked separately with explicit package paths for the current host platform:

```sh
cargo run -p stream-dock-manager-plugin-host --example validate_packages -- \
  --paths /absolute/path/to/package.zip
```

You can provide more ZIP paths after `--paths`. This example checks the expected first-party package contract without installing or executing the plugin; it is not a general validator for every vendor package. Keep these fixtures independent so both sides do not share the same packaging mistake.

[Plugin acceptance fixtures](../../../../tests/fixtures/plugins/README.md) · [Contributing](../../../../CONTRIBUTING.md)
