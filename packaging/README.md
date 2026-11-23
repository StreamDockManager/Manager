# Manager packaging

[Download Manager](https://github.com/StreamDockManager/Manager/releases/latest) · [Build from source](../README.md#run-from-source)

For a first install, choose your platform below, open Manager, then [set up your first control](../README.md#set-up-your-first-control). The CLI is optional and installs separately.

## Install an existing package

| Platform                    | Desktop    | CLI                       | Setup guide                                                     |
| --------------------------- | ---------- | ------------------------- | --------------------------------------------------------------- |
| macOS Intel / Apple Silicon | DMG        | DMG or portable ZIP       | [macOS](install-instructions/macos/INSTALL-INSTRUCTIONS.md)     |
| Windows x64                 | EXE        | EXE or portable ZIP       | [Windows](install-instructions/windows/INSTALL-INSTRUCTIONS.md) |
| Linux x64                   | DEB or RPM | DEB, RPM, or portable ZIP | [Linux](install-instructions/linux/INSTALL-INSTRUCTIONS.md)     |

Use the package matching your OS and architecture. Packages include installation instructions, the project license, and dependency notices. Node.js is managed separately through **Settings → Runtimes**.

**Packages have no trusted publisher signature, and macOS builds are not notarized.** The desktop app's macOS ad hoc seal checks bundle integrity but does not authenticate a publisher. Review source/build links and included checksums before installing.

## Build for your current platform

From the Manager checkout, install the [development prerequisites](../README.md#requirements), then run:

```sh
make deps
make package
```

| Command                                      | Default output                                                                        |
| -------------------------------------------- | ------------------------------------------------------------------------------------- |
| `make package`                               | Desktop installers under `target/release/bundle/`                                     |
| `make build-cli`                             | Standalone executable at `target/release/stream-dock-manager-cli` (`.exe` on Windows) |
| `make package-target TARGET=... PRODUCT=...` | Installer files in a fresh directory under `target/cross/<target>/`                   |

Paths assume Cargo's target directory is not overridden. A local build does not publish anything or qualify native behavior.

## Local target builds

Run these from the Manager checkout:

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

Run from macOS with Xcode Command Line Tools and the required Rust targets:

```sh
rustup target add aarch64-apple-darwin
make build-macos ARCH=arm64 PRODUCT=desktop
```

Use `x86_64-apple-darwin` and `ARCH=amd64` for Intel. `ARCH=both` is the default
and creates two separate installer sets, not a universal binary. Outputs go to a new
`target/cross/macos-*/installers-*/` directory. Cross-building Intel on Apple Silicon
does not test the installer on an Intel Mac.

## Build Linux packages from macOS

Start Docker with BuildKit and `linux/amd64` support, then run:

```sh
make build-linux PRODUCT=all
```

Use `PRODUCT=desktop` for DEB/RPM desktop packages or `PRODUCT=cli` for CLI DEB/RPM/ZIP.
The pinned Ubuntu 24.04 container handles dependencies and compilers; no Linux Rust target is needed on the Mac. Apple Silicon emulates amd64. Use `WORKSPACE_JOBS=2` to limit build concurrency and `docker info` or `docker buildx inspect --bootstrap` to diagnose Docker setup. Successful runs print the fresh `target/cross/linux-amd64/installers-*/` directory. CLI-only builds do not need Node.

## Installer metadata

[metadata.toml](metadata.toml) supplies the publisher, website, SPDX licenses,
copyright, descriptions, release date, and DEB/RPM fields for all installer builders.
From the Manager root, run `python3 scripts/package_metadata.py` to validate it.
For overrides, copy it under `packaging/` and set
`STREAM_DOCK_MANAGER_PACKAGE_METADATA=packaging/my-metadata.toml`.
Use the Make or supported installer wrappers; raw Cargo/Tauri/RPM commands can bypass
metadata processing. Do not put credentials here: resolved metadata ships in each package.

| Setting                                | Where it appears                                                                                              |
| -------------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| `package.publisher`, `package.website` | Maintainer, publisher, developer, and website fields where the platform supports them                         |
| `package.license`, `cli.license`       | Package and AppStream declarations; shipped project and third-party notices are preserved                     |
| `package.last_updated`                 | Optional `YYYY-MM-DD` release date in the package metadata and Linux AppStream, not the install or build time |
| `desktop.*`, `cli.*`, `linux.*`        | Product names/descriptions and package-specific policy                                                        |

Linux desktop packages install AppStream metadata and a copy of the released metadata
under `/usr/share/doc/stream-dock-manager/`. Desktop bundles also include
`notices/package-metadata.json`; CLI packages include resolved metadata in their README.
macOS and some RPM fields have no native equivalent and are still in the bundled
metadata. These declarations do not sign or relicense anything.

## First launch checks

1. Follow your OS guide above. On Linux, reconnect the dock so access rules apply.
2. Launch as a normal desktop user and complete setup.
3. On the target OS, verify a restart keeps settings, permissions work, and the dock sends input and receives display updates.

Install, upgrade and normal removal preserve user configuration. Direct interactive Windows desktop uninstall offers optional, confirmed permanent deletion; see the [Windows installer guide](windows/README.md). Cross-builds cannot qualify a native desktop session, device or installer lifecycle.

## Prepare and publish a release

1. Run **Actions → Prepare Release** on `main`. It builds and verifies artifacts without publishing.
2. Review the successful run and copy its run ID.
3. On the canonical product repository, run **Publish Release** on `main` with that
   `prepare_run_id`. It verifies and reuses `manager-publication` rather than rebuilding.

Both workflows are manual. A rehearsal cannot be published. Releases include checksums and third-party notices; private assets require GitHub authentication.
See [release and retry rules](../CONTRIBUTING.md#prepare-once-publish-the-same-files).

## Notes for maintainers

- Keep desktop and CLI payloads and catalogs separate. Preserve project and third-party notices.
- Linux packages install interface-specific USB access rules. Do not grant world-writable HID access.
- A Docker RPM recipe does not qualify native installation or hardware behavior.
- For retries, reuse the same verified files or prepare a fresh, complete run. Never replace
  a published version with different bytes.

## Manual Blacksmith binary builds

Run **Actions → blacksmith-build → Run workflow**, choose the source ref, then
choose exactly one `product` (`desktop` or `cli`) and one native `target`:

| Target          | Blacksmith runner                  |
| --------------- | ---------------------------------- |
| `linux-amd64`   | `blacksmith-4vcpu-ubuntu-2404`     |
| `linux-arm64`   | `blacksmith-4vcpu-ubuntu-2404-arm` |
| `windows-amd64` | `blacksmith-4vcpu-windows-2025`    |
| `macos-arm64`   | `blacksmith-6vcpu-macos-15`        |

The [official runner inventory](https://docs.blacksmith.sh/blacksmith-runners/overview)
lists macOS on Apple Silicon only and Windows runners as public beta. There is no
Intel Mac choice or silent GitHub-hosted fallback. The Blacksmith GitHub App must
be enabled for **each repository** using the workflow, with runner access and
billing available there. Source synchronization does not transfer that account
setup. The workflow is repository-neutral and can be synced without edits.

One job builds only the selected binary. CLI skips Node/frontend setup; desktop
uses the existing Tauri build wrapper with bundling disabled. There is no test
matrix, installer packaging, release preparation, tag, publication or deployment.
Patched dependency integrity checks and a native OS/architecture check remain.
Download the `blacksmith-<product>-<target>-<commit>` Actions artifact and extract
the contained `.tar.gz` (which preserves executable permissions).

These are raw candidate binaries, not portable installers or release-qualified
packages. Linux builds use Ubuntu 24.04 and require matching shared libraries. Desktop needs
WebKitGTK on Linux and WebView2 on Windows. Raw desktop binaries do not install
udev rules, file associations, launch services, or a macOS application bundle.
Use the existing installer/release workflows for those integration steps.

[Back to Manager](../README.md)
