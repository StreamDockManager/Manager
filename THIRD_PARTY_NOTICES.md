# Third-party notices

The application is GPL-3.0-or-later. Dependency licenses remain their respective owners' licenses. Source dependencies and exact versions are recorded in Cargo.lock and package-lock.json.

Managed state uses rusqlite 0.41.1 (MIT), libsqlite3-sys 0.39.1 (MIT), and bundled SQLite 3.54.1 (public domain). The exact crate source checksums are in Cargo.lock. Preserve rusqlite/libsqlite3-sys license notices in release source inventories. SQLite is compiled into the shared Rust host; plugins do not bundle another database driver. See [SQLite copyright](https://www.sqlite.org/copyright.html) and [release history](https://www.sqlite.org/changes.html).

Alpine.js / @alpinejs/csp is MIT licensed. AlpineComponentLoader is MIT licensed, copyright LouisT; pinned source is preserved by the dependency lock and its license is retained in the copied distribution. Tauri is MIT OR Apache-2.0 licensed. Rust's standard library and compiler are MIT OR Apache-2.0 licensed.

The Windows NSIS installer template is derived from the template embedded in Tauri CLI 2.12.4, copyright (c) 2017 - Present Tauri Apps Contributors, under the MIT license. Its full notice is retained in `packaging/windows/TAURI-LICENSE-MIT`; the original source hash and local safety changes are documented alongside it. Windows release packages include that notice under `notices/TAURI-LICENSE-MIT`.

Public N4 protocol references are linked from the device README. Expansion protocol facts are independently implemented from the revision-pinned MIT-licensed MiraboxSpace Device SDK; its full notice is preserved in `hardware/MiraboxSpace-LICENSE.txt` and source identities/methods in `hardware/protocol-sources.v1.json`. This project does not distribute VSD Craft, proprietary SDK transport libraries, firmware, vendor assets or external plugin packages. Acceptance packages must retain their upstream licenses and pinned revisions before redistribution.

Downloaded acceptance packages retain their pinned source licenses in ignored artifacts. Preserve complete dependency license inventories for each release architecture.

MP4 import invokes an externally installed FFmpeg executable. This repository and the development application bundle do not redistribute FFmpeg. Linux packages declare it as a system dependency. An FFmpeg build's enabled components determine its applicable licenses; any future bundled build must preserve its licenses, notices and required corresponding source. GIF decoding uses the Rust `image`/`gif` dependencies recorded in Cargo.lock.

## Appearance assets

- Noto Sans and Noto Sans Mono: bundled TrueType fonts from Google Fonts, under the SIL Open Font License 1.1. Full notices are in `public/appearance/fonts/OFL-NotoSans.txt` and `OFL-NotoSansMono.txt`.
- Lucide icons: selected SVGs from release 0.468.0, under the ISC license. Full notice is in `public/appearance/icons/LICENSE`. The `home.svg` asset is the release's `house.svg`.
- Source URLs and SHA-256 hashes for the bundled assets are recorded in `public/appearance/assets.json`.

## Release discovery and verification

The Rust update service uses reqwest, rustls and its cryptographic/platform-verification dependencies for HTTPS, RustCrypto sha2 for checksums, and semver for version ordering. Their exact versions, dependency graph and upstream source checksums are in Cargo.lock. Preserve their published license/notice files in any dependency source inventory used for redistribution. No GitHub credentials are embedded in the application.

The optional Node.js LTS runtime downloader uses flate2 (MIT/Apache-2.0) and tar (MIT/Apache-2.0), with their Cargo.lock-pinned transitive dependencies. Downloads use official HTTPS sources and retain upstream Node.js license and dependency notices. Node.js is not included in application installers.

Plugin, runtime, profile, and media ZIP handling shares `zip` 9.0.5 (MIT) with Deflate and LZMA decoding. LZMA decoding and end-of-stream validation use `lzma-rust2` 0.22.0 (Apache-2.0). Their exact source checksums and compression dependencies are recorded in Cargo.lock and must be included in release dependency/license inventories.

Desktop startup registration on macOS/Windows uses `tauri-plugin-autostart` 2.7.0 (MIT OR Apache-2.0) and its locked `auto-launch` 0.6.0 dependency (MIT); exact transitive versions/checksums are in Cargo.lock. Preserve their upstream notices in the dependency inventory.

The packaged video preparation worker includes Mediabunny 1.62.1 (MPL-2.0), from
https://github.com/Vanilagy/mediabunny. Its unmodified source is available in the
corresponding npm package and upstream release. The MPL-2.0 notice is included in
`licenses/mediabunny-MPL-2.0.txt` in frontend builds. It runs locally and supplies
MP4 frame extraction only; browser codec availability is detected at runtime.

The codec refresh references MiraboxSpace/StreamDock-Device-SDK revision
`fc124cb5bc8fe0cd6c72d3f2af80588adc7dcffb`, including its Python image helpers and
model definitions, under the preserved MIT notice. Historical source identities and
independent retail N1/N3 source notices remain retained in the hardware inventories.
No vendor binary or firmware is redistributed.

## Plugin compatibility evidence

Revision-pinned Elgato and VSD plugin SDK source snippets and samples under
`tests/fixtures/plugins/vendor/` retain their MIT licenses and original copyright
headers. The VSD C++ core source also retains its Corsair copyright notices.
These files are read-only source evidence, not vendor executables or bundled
applications. The exact published `@elgato/schemas` 0.6.0 manifest JSON Schema is
also embedded for runtime shape validation; its original MIT notice is retained
in `vendor/elgato/manifest-schema/LICENSE` beneath that fixture directory. Source identities and SHA-256 hashes are recorded in
`tests/fixtures/plugins/vendor-sources.json`. The VSD documentation index contains
factual API/member identifiers and requiredness annotations with source URLs and
response hashes; website prose and scripts are not redistributed.

Elgato-recommended `sdpi-components` 4.0.1 file component, path sanitizer and
upstream tests are retained as untouched MIT source evidence in
`tests/fixtures/plugins/vendor/elgato/sdpi-components/`, including its
Copyright (c) 2024 Corsair Memory Inc. license. They are not bundled as a
production UI dependency.

### Embedded @elgato/schemas 0.6.0 manifest schema

The runtime embeds the published Elgato manifest JSON Schema under this notice:

MIT License

Copyright (c) Corsair Memory Inc.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
