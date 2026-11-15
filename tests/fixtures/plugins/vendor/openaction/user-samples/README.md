# User-supplied import regressions

These manifests were supplied as reproduction samples on 2026-11-09 and are
preserved byte-for-byte. Only manifest metadata is included; no executable,
inspector, image, credential or other package content is redistributed. Their
upstream repository revisions and licenses have not been independently verified.
They are not covered by the neighboring OpenDeck starterpack license notice.

## How to use this folder

These are manifest-only regression inputs. Do not install or run them as plugins. Manager tests supply disposable placeholder files and validate import behavior without redistributing the original packages.

| Sample                    | Import case                                       |
| ------------------------- | ------------------------------------------------- |
| `system-information.json` | Numeric font sizes and legacy metadata            |
| `redline.json`            | Four-component version and Linux-only declaration |

Keep original bytes and the hashes below unchanged. See the [OpenAction fixture guide](../README.md) for the wider protocol inventory.

- `system-information.json`: original `sample1/manifest.json`, System Information
  by nekename, version 1.0.0, package identity `me.amankhanna.oasystem`. SHA-256:
  `44e94c30ac72faa100a31b85e3e1a09963f516ee6a383df3053240b322eba3f2`.
- `redline.json`: original `sample2/manifest(1).json`, Redline Monitor by Ming,
  version 1.0.9.1, package identity `com.kahikara.opendeck-redline`. SHA-256:
  `f5a1b66235220e0e0054bf066aa3b9a12278aa01d9dd329271fd3594587ebe08`.

The current OpenAction manifest documentation specifies SemVer and string font
sizes. These fixtures exercise the explicitly bounded OpenDeck import adapter:
integer font sizes, four-component numeric versions, `Software.MinimumVersion`
metadata and the legacy OS `MinimumVersion` alias. They do not redefine the
OpenAction specification. Tests create disposable placeholder files, never run
these plugins, and preserve Redline's Linux-only declaration. Shape acceptance
is not permission or evidence to run Linux binaries on macOS or Windows.

Specification: https://github.com/OpenActionAPI/docs/blob/ffe2564bd42f54b19443cf1dac821adf6a85a848/src/manifest.md

Reference implementation evidence: OpenDeck commit
`3ddecc1ce8952a423916cf066724356267b97410`,
[`src-tauri/src/plugins/manifest.rs`](https://github.com/nekename/OpenDeck/blob/3ddecc1ce8952a423916cf066724356267b97410/src-tauri/src/plugins/manifest.rs)
represents plugin version as a string and ignores unmodeled Software metadata;
[`src-tauri/src/shared.rs`](https://github.com/nekename/OpenDeck/blob/3ddecc1ce8952a423916cf066724356267b97410/src-tauri/src/shared.rs)
accepts unsigned-integer or string FontSize values. Manager intentionally keeps a
narrower bounded adapter rather than accepting arbitrary versions or fields.
