# OpenAction SDK test fixture

Opt-in transport qualification against the official Rust SDK 2.8.0, pinned to
[OpenActionAPI/rust commit d435b2d57d6df13debaca330011a3067f66a99db](https://github.com/OpenActionAPI/rust/tree/d435b2d57d6df13debaca330011a3067f66a99db).
The upstream SDK is MIT licensed; it is fetched as a dependency, not copied here.
The probe source is Manager-owned. It uses only disposable files and the local
host WebSocket. No installed plugins, device hardware, browser or external service
is opened. This is transport evidence, not physical-device or browser qualification.

## Requirements

| Requirement                           | Why                             |
| ------------------------------------- | ------------------------------- |
| Rust/Cargo and Manager's dependencies | Build the fixture and host      |
| Access to the pinned SDK dependency   | Resolve the locked build inputs |
| Local loopback sockets                | Connect to the disposable host  |

## Build and run

From the **Manager repository root**, after setting up the [contributor prerequisites](../../../../CONTRIBUTING.md):

```sh
cargo build --locked --manifest-path tests/fixtures/plugins/openaction-sdk-executable/Cargo.toml
STREAM_DOCK_MANAGER_OPENACTION_FIXTURE="$PWD/tests/fixtures/plugins/openaction-sdk-executable/target/debug/manager-openaction-sdk-probe" \
  cargo test -p stream-dock-manager-plugin-host --test openaction_sdk -- --ignored
```

The shown executable path assumes Cargo's default target directory. If you use `CARGO_TARGET_DIR`, point `STREAM_DOCK_MANAGER_OPENACTION_FIXTURE` at the actual build output. On Windows, use the executable's absolute `.exe` path. The fixture is not installed into your everyday Manager configuration.

The integration test copies
it into a temporary `.sdPlugin` package before launching it through the production
host. Registration, lifecycle, arbitrary JSON settings, keypad state transition,
encoder input, null-state visuals, plugin settings and bidirectional inspector
messages are checked against genuine SDK deserialization and event dispatch.

[All plugin fixtures](../README.md) · [Manager setup](../../../../README.md)
