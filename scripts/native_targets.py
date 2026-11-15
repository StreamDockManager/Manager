"""Manager-owned native target inventory shared by build and packaging tools."""
from dataclasses import dataclass


@dataclass(frozen=True)
class NativeTarget:
    os: str
    arch: str
    system: str
    runner_arch: str
    rust: str
    desktop_bundles: str
    installer_formats: tuple[str, ...]

    # Resolve the canonical name for the selected native target
    @property
    def name(self):
        return f'{self.os}-{self.arch}'


NATIVE_TARGETS = (
    NativeTarget('windows', 'amd64', 'Windows', 'X64', 'x86_64-pc-windows-msvc', 'nsis', ('.exe',)),
    NativeTarget('macos', 'amd64', 'Darwin', 'X64', 'x86_64-apple-darwin', 'app,dmg', ('.dmg',)),
    NativeTarget('macos', 'arm64', 'Darwin', 'ARM64', 'aarch64-apple-darwin', 'app,dmg', ('.dmg',)),
    NativeTarget('linux', 'amd64', 'Linux', 'X64', 'x86_64-unknown-linux-gnu', 'deb,rpm', ('.deb', '.rpm')),
)
NATIVE_PLATFORMS = frozenset((target.os, target.arch) for target in NATIVE_TARGETS)
INSTALLER_FORMATS = {target.os: target.installer_formats for target in NATIVE_TARGETS}
INSTALLER_VARIANTS = frozenset((target.os, target.arch, extension)
                              for target in NATIVE_TARGETS for extension in target.installer_formats)
PORTABLE_VARIANTS = frozenset((target.os, target.arch, '.zip') for target in NATIVE_TARGETS)
