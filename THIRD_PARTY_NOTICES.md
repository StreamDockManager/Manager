# Third-party notices

The application is GPL-3.0-or-later. Dependency licenses remain their respective owners' licenses. Source dependencies and exact versions are recorded in Cargo.lock and package-lock.json.

The Linux clipboard adapters use x11rb (MIT OR Apache-2.0) and the Wayland client, wayland-protocols and wayland-protocols-wlr crates (MIT). Preserve their upstream license notices and those of the bundled protocol definitions in release inventories.

Managed state uses rusqlite 0.43.0 (MIT), libsqlite3-sys 0.40.0 (MIT), and SQLCipher 4.20.0 Community Edition (BSD-3-Clause), based on SQLite 3.54.1 (public domain). The build uses the sqlcipher-src amalgamation and vendored OpenSSL 3.6.3 (Apache-2.0). The exact crate source checksums are in Cargo.lock. Preserve rusqlite/libsqlite3-sys license notices in release source inventories. SQLite is compiled into the shared Rust host; plugins do not bundle another database driver. The SQLCipher copyright notice and terms are included in [SQLCipher licensing](https://www.zetetic.net/sqlcipher/license/). See [OpenSSL licensing](https://openssl-library.org/policies/general/license/), [SQLite copyright](https://www.sqlite.org/copyright.html) and [release history](https://www.sqlite.org/changes.html).

Alpine.js / @alpinejs/csp is MIT licensed. AlpineComponentLoader is MIT licensed, copyright LouisT; pinned source is preserved by the dependency lock and its license is retained in the copied distribution. Tauri is MIT OR Apache-2.0 licensed. Rust's standard library and compiler are MIT OR Apache-2.0 licensed.

The Windows NSIS installer template is derived from the template embedded in Tauri CLI 2.12.6, copyright (c) 2017 - Present Tauri Apps Contributors, under the MIT license. Its full notice is retained in `packaging/windows/TAURI-LICENSE-MIT`; the original source hash and local safety changes are documented alongside it. Windows release packages include that notice under `notices/TAURI-LICENSE-MIT`.

Public N4 protocol references are linked from the device README. Expansion protocol facts are independently implemented from the revision-pinned MIT-licensed MiraboxSpace Device SDK; its full notice is preserved in `hardware/MiraboxSpace-LICENSE.txt` and source identities/methods in `hardware/protocol-sources.v1.json`. This project does not distribute VSD Craft, proprietary SDK transport libraries, firmware, vendor assets or external plugin packages. Acceptance packages must retain their upstream licenses and pinned revisions before redistribution.

Downloaded acceptance packages retain their pinned source licenses in ignored artifacts. Preserve complete dependency license inventories for each release architecture.

The desktop Video Editor uses the local Mediabunny/WebCodecs worker for MP4 preparation when supported. Its fallback and CLI MP4 import use an externally installed FFmpeg; it is not redistributed by this project. Some Linux packages declare or recommend it as a system dependency. An FFmpeg build's enabled components determine its applicable licenses; any future bundled build must preserve its licenses, notices and required corresponding source. GIF decoding uses the Rust `image`/`gif` dependencies recorded in Cargo.lock.

## Wallpaper JPEG encoder

Automatic animated wallpaper output on the VSDInside N4 Pro uses jpeg-encoder 0.7.1 (MIT or Apache-2.0, with IJG-licensed DCT code). This software is based in part on the work of the Independent JPEG Group. [jpeg-encoder source and licenses](https://github.com/vstroebel/jpeg-encoder)

```text
Copyright (c) 2021 Volker Ströbel <volkerstroebel@mysurdity.de>

Permission is hereby granted, free of charge, to any
person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the
Software without restriction, including without
limitation the rights to use, copy, modify, merge,
publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software
is furnished to do so, subject to the following
conditions:

The above copyright notice and this permission notice
shall be included in all copies or substantial portions
of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE.
```

## Appearance assets

- Inter 4.1: unmodified variable WOFF2 browser fonts and static Regular/Bold/Italic/Bold Italic TrueType fonts from the [official rsms/inter release](https://github.com/rsms/inter/releases/tag/v4.1), under SIL OFL-1.1. Copyright (c) 2016 The Inter Project Authors. The complete upstream notice is in `public/appearance/fonts/OFL-Inter.txt` and reproduced below so every package carrying this notices file retains it. Archive and individual asset hashes are pinned in `public/appearance/assets.json`; no runtime font download is required.
- Lucide icons: selected SVGs from release 0.468.0, under the ISC license, with Feather-derived icons retaining their MIT attribution. Both notices are in `public/appearance/icons/LICENSE`. The `home.svg` asset is the release's `house.svg`.
- Source URLs and SHA-256 hashes for the bundled assets are recorded in `public/appearance/assets.json`.

- Natural Earth 5.1.2: the offline Vault location map uses simplified outlines derived from [Natural Earth's 1:110m land vectors](https://www.naturalearthdata.com/downloads/110m-physical-vectors/110m-land/). The data is [public domain](https://www.naturalearthdata.com/about/terms-of-use/). Source and generation details are in `src/map-data/README.md`.

## Release discovery and verification

The Rust update service uses reqwest, rustls and its cryptographic/platform-verification dependencies for HTTPS, RustCrypto sha2 for checksums, and semver for version ordering. Their exact versions, dependency graph and upstream source checksums are in Cargo.lock. Preserve their published license/notice files in any dependency source inventory used for redistribution. No GitHub credentials are embedded in the application.

The optional Node.js LTS runtime downloader uses flate2 (MIT/Apache-2.0) and tar (MIT/Apache-2.0), with their Cargo.lock-pinned transitive dependencies. Downloads use official HTTPS sources and retain upstream Node.js license and dependency notices. Node.js is not included in application installers.

Plugin, runtime, profile, and media ZIP handling shares `zip` 9.8.0 (MIT) with Deflate and LZMA decoding. LZMA decoding and end-of-stream validation use `lzma-rust2` 0.25.0 (Apache-2.0). Their exact source checksums and compression dependencies are recorded in Cargo.lock and must be included in release dependency/license inventories.

Desktop startup registration on macOS/Windows uses `tauri-plugin-autostart` 2.7.0 (MIT OR Apache-2.0) and its locked `auto-launch` 0.6.0 dependency (MIT); exact transitive versions/checksums are in Cargo.lock. Preserve their upstream notices in the dependency inventory.

The packaged video preparation worker includes [Mediabunny 1.66.2](https://github.com/Vanilagy/mediabunny/releases/tag/v1.66.2) (MPL-2.0). Its unmodified source is available in the corresponding npm package and upstream release. The MPL-2.0 notice is included in `licenses/mediabunny-MPL-2.0.txt` in frontend builds. The worker runs locally and uses it to index MP4 frame timestamps and extract frames for previews and trimmed clips. Decoding uses the webview's WebCodecs support, which is detected at runtime.

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

## Inter 4.1 font license

```text
Copyright (c) 2016 The Inter Project Authors (https://github.com/rsms/inter)

This Font Software is licensed under the SIL Open Font License, Version 1.1.
This license is copied below, and is also available with a FAQ at:
http://scripts.sil.org/OFL

-----------------------------------------------------------
SIL OPEN FONT LICENSE Version 1.1 - 26 February 2007
-----------------------------------------------------------

PREAMBLE
The goals of the Open Font License (OFL) are to stimulate worldwide
development of collaborative font projects, to support the font creation
efforts of academic and linguistic communities, and to provide a free and
open framework in which fonts may be shared and improved in partnership
with others.

The OFL allows the licensed fonts to be used, studied, modified and
redistributed freely as long as they are not sold by themselves. The
fonts, including any derivative works, can be bundled, embedded,
redistributed and/or sold with any software provided that any reserved
names are not used by derivative works. The fonts and derivatives,
however, cannot be released under any other type of license. The
requirement for fonts to remain under this license does not apply
to any document created using the fonts or their derivatives.

DEFINITIONS
"Font Software" refers to the set of files released by the Copyright
Holder(s) under this license and clearly marked as such. This may
include source files, build scripts and documentation.

"Reserved Font Name" refers to any names specified as such after the
copyright statement(s).

"Original Version" refers to the collection of Font Software components as
distributed by the Copyright Holder(s).

"Modified Version" refers to any derivative made by adding to, deleting,
or substituting -- in part or in whole -- any of the components of the
Original Version, by changing formats or by porting the Font Software to a
new environment.

"Author" refers to any designer, engineer, programmer, technical
writer or other person who contributed to the Font Software.

PERMISSION AND CONDITIONS
Permission is hereby granted, free of charge, to any person obtaining
a copy of the Font Software, to use, study, copy, merge, embed, modify,
redistribute, and sell modified and unmodified copies of the Font
Software, subject to the following conditions:

1) Neither the Font Software nor any of its individual components,
in Original or Modified Versions, may be sold by itself.

2) Original or Modified Versions of the Font Software may be bundled,
redistributed and/or sold with any software, provided that each copy
contains the above copyright notice and this license. These can be
included either as stand-alone text files, human-readable headers or
in the appropriate machine-readable metadata fields within text or
binary files as long as those fields can be easily viewed by the user.

3) No Modified Version of the Font Software may use the Reserved Font
Name(s) unless explicit written permission is granted by the corresponding
Copyright Holder. This restriction only applies to the primary font name as
presented to the users.

4) The name(s) of the Copyright Holder(s) or the Author(s) of the Font
Software shall not be used to promote, endorse or advertise any
Modified Version, except to acknowledge the contribution(s) of the
Copyright Holder(s) and the Author(s) or with their explicit written
permission.

5) The Font Software, modified or unmodified, in part or in whole,
must be distributed entirely under this license, and must not be
distributed under any other license. The requirement for fonts to
remain under this license does not apply to any document created
using the Font Software.

TERMINATION
This license becomes null and void if any of the above conditions are
not met.

DISCLAIMER
THE FONT SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO ANY WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT
OF COPYRIGHT, PATENT, TRADEMARK, OR OTHER RIGHT. IN NO EVENT SHALL THE
COPYRIGHT HOLDER BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
INCLUDING ANY GENERAL, SPECIAL, INDIRECT, INCIDENTAL, OR CONSEQUENTIAL
DAMAGES, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
FROM, OUT OF THE USE OR INABILITY TO USE THE FONT SOFTWARE OR FROM
OTHER DEALINGS IN THE FONT SOFTWARE.
```

## SQLCipher Community Edition

```text
Copyright (c) 2025, ZETETIC LLC
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:
    * Redistributions of source code must retain the above copyright
      notice, this list of conditions and the following disclaimer.
    * Redistributions in binary form must reproduce the above copyright
      notice, this list of conditions and the following disclaimer in the
      documentation and/or other materials provided with the distribution.
    * Neither the name of the ZETETIC LLC nor the
      names of its contributors may be used to endorse or promote products
      derived from this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY ZETETIC LLC ''AS IS'' AND ANY
EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL ZETETIC LLC BE LIABLE FOR ANY
DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
(INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND
ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```
