# Build and install Manager packages

Use this guide to create local installers or install a package you already have. The project is **unreleased at 0.1.0**; no public download is assumed. Desktop and CLI packages share the project version, but the desktop app does not require or bundle the CLI.

## Choose your task

| I want to…                            | Start here                                                 |
| ------------------------------------- | ---------------------------------------------------------- |
| Install a package I have              | [Platform guides](#install-an-existing-package)            |
| Package my current checkout           | [Current-platform build](#build-for-your-current-platform) |
| Build Ubuntu packages on a Mac        | [Docker Linux build](#build-linux-packages-from-macos)     |
| Build another CPU or operating system | [Target builds](#local-target-builds)                      |
| Verify an installer                   | [First-launch checks](#first-launch-checks)                |

> Building a package does not publish it. Keep installer generation separate from running it against your real configuration.

## Install an existing package

| Platform                    | Desktop    | CLI                       | Setup guide                                                     |
| --------------------------- | ---------- | ------------------------- | --------------------------------------------------------------- |
| macOS Intel / Apple Silicon | DMG        | DMG or portable ZIP       | [macOS](install-instructions/macos/INSTALL-INSTRUCTIONS.md)     |
| Windows x64                 | EXE        | EXE or portable ZIP       | [Windows](install-instructions/windows/INSTALL-INSTRUCTIONS.md) |
| Linux x64                   | DEB or RPM | DEB, RPM, or portable ZIP | [Linux](install-instructions/linux/INSTALL-INSTRUCTIONS.md)     |

Use the package matching your OS and architecture. Packages include installation instructions, the project license, and dependency notices. Node.js is managed separately through **Settings → Runtimes**.

**Packages have no trusted publisher signature, and macOS builds are not notarized.** The desktop app's macOS ad hoc seal checks bundle integrity but does not authenticate a publisher. Review source/build links and included checksums before installing.

## Installer metadata

Edit [`metadata.toml`](metadata.toml) before building. This single file supplies the
publisher/developer name, homepage, project/package licenses, copyright, descriptions,
release date, Debian section/priority, and RPM release revision. It applies to desktop
and CLI installer builds on Linux, macOS, and Windows, including the Docker Linux path.
Versions and application/package identifiers still come from the existing release sources.
Changing a declaration does **not** relicense the source or third-party dependencies,
authenticate a publisher, sign a package, or notarize a macOS application.

```sh
python3 scripts/package_metadata.py  # Validate metadata without building
make build-linux PRODUCT=all         # Both desktop and CLI DEB/RPM packages
```

For a build-specific configuration, copy the entire file somewhere under `packaging/`
and set `STREAM_DOCK_MANAGER_PACKAGE_METADATA=packaging/my-metadata.toml` before the
normal build command. Fields and SPDX expression syntax are validated; misspelled/unknown keys are rejected.
License identifiers and legal correctness still require maintainer review.
Docker builds forward that relative path and include the configuration in their build
context. Do not put secrets in this file: its resolved contents are shipped in packages.
Direct raw `cargo`, Tauri CLI, or `rpmbuild` commands bypass these wrappers; use the
repository's Make commands, `scripts/tauri.mjs`, or `scripts/package_cli.py`.

| Metadata                         | Where it is used                                                                                                                                                                              |
| -------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `package.publisher`              | Desktop bundle publisher (Windows installer publisher and Debian maintainer); AppStream developer; CLI Windows Publisher and Linux maintainer/vendor/packager                                 |
| `package.website`                | Desktop/CLI Linux homepage, Windows installer information URL where supported, AppStream homepage                                                                                             |
| `package.license`, `cli.license` | Desktop bundle / CLI RPM license declaration; AppStream project license; full declarations retained in the included metadata document. Existing LICENSE and third-party notices are preserved |
| `package.copyright`              | Desktop bundle copyright where supported; included metadata document                                                                                                                          |
| `package.last_updated`           | Optional release date in desktop AppStream and the included metadata document; **not** build time or installation time                                                                        |
| `desktop.*`                      | AppStream display name/developer identifier and desktop descriptions                                                                                                                          |
| `cli.summary`, `cli.description` | CLI DEB/RPM descriptions                                                                                                                                                                      |
| `linux.*`                        | Debian section/priority and RPM release revision                                                                                                                                              |

Use `last_updated = "YYYY-MM-DD"` for a deliberate release date, or leave it empty
while unreleased. Empty dates produce no AppStream release entry. DEB has no standard
"last updated" control field; RPM BuildTime and OS installation dates remain tool/OS-owned.
Tauri does not currently populate the desktop RPM Vendor/Packager tags; the publisher
is available through AppStream and the included metadata document. The CLI RPM sets
Vendor/Packager directly. Software centers decide which fields to show and may need refreshed caches. macOS has
no standard publisher website or last-updated installer field, so these are retained in
the included metadata document rather than presented as a native field.

Desktop packages include `notices/package-metadata.json` as a bundle resource; DEB/RPM
also install it under `/usr/share/doc/stream-dock-manager/`. CLI installers and portable ZIPs retain the same resolved metadata in their existing
`README.md` (Linux: `/usr/share/doc/stream-dock-manager-cli/README.md`).
Both desktop Linux formats install generated AppStream metadata at
`/usr/share/metainfo/app.streamdockmanager.manager.metainfo.xml`.

## Build for your current platform

Install the [development prerequisites](../README.md#requirements), clone the Manager repository, and open a terminal in its root directory. The commands below use GNU Make and Python 3.11+. For a desktop installer:

```sh
make deps
make package
```

For only the CLI executable:

```sh
make build-cli
```

| Command                 | Default output                                               | Product                                     |
| ----------------------- | ------------------------------------------------------------ | ------------------------------------------- |
| `make package`          | `target/release/bundle/`                                     | Desktop installers for the current platform |
| `make build-cli`        | `target/release/stream-dock-manager-cli` (`.exe` on Windows) | CLI executable only, not a CLI installer    |
| `make package-target …` | A fresh directory under `target/cross/<target>/`             | Selected desktop and/or CLI packages        |

These paths assume Cargo's target directory has not been overridden. A release-mode build creates local files; it does not publish a release or establish native qualification. To package the CLI, use a target build below.

## Local target builds

Run these from the Manager repository root:

| Target              | Command                                                |
| ------------------- | ------------------------------------------------------ |
| Windows x64         | `make package-target TARGET=windows-amd64 PRODUCT=all` |
| macOS Apple Silicon | `make package-target TARGET=macos-arm64 PRODUCT=all`   |
| macOS Intel         | `make package-target TARGET=macos-amd64 PRODUCT=all`   |
| Linux x64           | `make package-target TARGET=linux-amd64 PRODUCT=all`   |

Set `PRODUCT=desktop` or `PRODUCT=cli` to select one. For `make package-target`, omitting `PRODUCT` selects desktop only. The `make build-macos` and `make build-linux` convenience targets default to both products. The command prints a fresh output directory under `target/cross/<target>/`.

| Build target           | Additional tools                                                                                 |
| ---------------------- | ------------------------------------------------------------------------------------------------ |
| Windows on Windows     | Native Rust/MSVC, C++ Build Tools, and Windows SDK                                               |
| Windows on macOS/Linux | Windows MSVC Rust target, `cargo-xwin`, NSIS, and LLVM tools (`clang-cl`, `lld-link`, `llvm-rc`) |
| macOS                  | macOS with Xcode Command Line Tools and the selected Rust target                                 |
| Linux x64              | Docker with BuildKit and `linux/amd64` support                                                   |

Local Linux builds use the pinned Ubuntu 24.04 container; Apple Silicon hosts use amd64 emulation. macOS installers must be built on macOS. Provision frontend dependencies before local Windows/macOS desktop builds.

## Select a macOS architecture

Run these commands from this repository on a Mac with Xcode Command Line Tools:

| Build                        | Command                                            |
| ---------------------------- | -------------------------------------------------- |
| Apple Silicon only           | `make build-macos ARCH=arm64`                      |
| Intel only                   | `make build-macos ARCH=amd64`                      |
| Both architectures (default) | `make build-macos` or `make build-macos ARCH=both` |

`make build-macos-arm64` and `make build-macos-amd64` are equivalent single-architecture
aliases. Builds default to desktop and CLI installers; add `PRODUCT=desktop` or `PRODUCT=cli`
to select one. Both means two separate installer sets, not one universal binary. Existing
`make build`, `make build-cli`, and `make package` remain native-host commands.

Install only the Rust targets you intend to build:

```sh
rustup target add aarch64-apple-darwin  # arm64
rustup target add x86_64-apple-darwin   # amd64
make build-macos ARCH=arm64 PRODUCT=desktop
```

Installers are written under `target/cross/macos-arm64/installers-*/` or
`target/cross/macos-amd64/installers-*/`. Building one architecture does not build the other.
Cross-building an Intel binary on Apple Silicon does not qualify native Intel runtime behavior.

## Build Linux packages from macOS

Start Docker Desktop and wait until its Linux engine is ready, then run from this repository:

```sh
make build-linux                         # desktop DEB/RPM and CLI DEB/RPM/ZIP
make build-linux PRODUCT=desktop         # only the desktop installers
make build-linux PRODUCT=cli             # only the CLI packages
```

This is a containerized Linux release build, not a direct macOS-to-Linux linker invocation.
The selected Docker Buildx builder must advertise `linux/amd64`; Apple Silicon uses emulation
and can take substantially longer than an Intel Linux builder. Preflight checks Docker, Buildx
and amd64 support before sending the source context. Use `docker info` and
`docker buildx inspect --bootstrap` to diagnose engine/builder setup. The command does not
install Docker, change its configuration or enable privileged emulation automatically.

The container supplies Rust, Node.js, GTK/WebKitGTK, Linux linkers and packaging tools. Before packaging, it runs the release-target compiler syntax/type and local-warning gate; desktop frontend assets are built first, while CLI-only builds remain independent of Node dependencies. Warnings from Manager or its local dependency patches fail the build rather than being buried in the package log.
Installing a Linux target with `rustup` on the Mac is **not** required for this route and,
on its own, cannot produce the complete desktop release. Dependencies are downloaded on
first build; subsequent builds reuse Docker layers and builder-owned Cargo caches. Concurrent builds
serialize access to the shared Cargo caches. Host `node_modules`, Cargo outputs,
artifacts and local configuration are excluded from the source context. No host directories
are mounted writable into the build container.

Successful builds print a fresh `target/cross/linux-amd64/installers-*` directory, containing
separate `desktop/` and `cli/` packages. Failed builds do not publish that installer directory.
`make build-linux WORKSPACE_JOBS=2` bounds Cargo parallelism. Docker owns its layer cache;
project cache-cleanup commands do not remove Docker images or builder caches.

These are unsigned local packages, not a published release. Building on Ubuntu 24.04 does
not qualify older distributions, a real graphical session, device access or installer lifecycle.
Test the resulting packages on the Linux release targets before publishing.

## First launch checks

1. Install as described in the package's OS guide.
2. On Linux, reconnect the device after access rules are installed.
3. Launch as the normal desktop user and complete setup.
4. Check that profiles and settings survive an app restart.
5. Verify device input, display output, permissions, and plugin behavior on the actual target platform.

Installation, upgrades, and ordinary removal preserve user configuration. A direct interactive Windows desktop uninstall can optionally remove the default shared Manager/CLI configuration after explicit confirmation; the choice is unchecked by default. See the [Windows uninstall safeguards and acceptance checks](windows/README.md). A cross-build or container test does not qualify a desktop session, physical hardware, or installer lifecycle.

## Prepare and publish a release

Local package commands above do not publish. On GitHub, run **Prepare Release** from
`main`, review the successful run, then separately run **Publish Release** from `main`
with its `prepare_run_id`. Publish reuses the verified `manager-publication` artifact;
it does not rebuild installers. Both require separate manual dispatches on `main` plus successful builds, package integrity and
provenance validation, not exhaustive native/vendor/physical qualification. Release notes
disclose partial compatibility. See the [release steps and retry rules](../CONTRIBUTING.md#prepare-once-publish-the-same-files).
Private releases require authenticated downloads; publication does not make the
repository public or establish private-release updater support.

## Notes for maintainers

| Area             | Requirement                                                                                                 |
| ---------------- | ----------------------------------------------------------------------------------------------------------- |
| Notices          | Preserve GPL and all applicable third-party license files                                                   |
| Native packaging | Keep desktop and CLI payloads and catalogs separate                                                         |
| Linux access     | Use generated interface-specific udev rules; do not grant world-writable HID access                         |
| Media            | Linux packages declare FFmpeg; macOS/Windows fallback uses an installed executable                          |
| Source archives  | Include locked dependencies and the provisioned caches needed by offline builds                             |
| Release recovery | Reuse the original verified artifacts; do not replace them with a new build under the same release identity |

The Nobara RPM spec is a reproducible-build recipe, not evidence of native qualification. Preserve its pinned build environment and source provenance. For CI branches, native runner requirements, and publication/recovery procedures, see [CONTRIBUTING.md](../CONTRIBUTING.md) and the checked-in [workflows](../.github/workflows/).

[Back to Manager](../README.md)
