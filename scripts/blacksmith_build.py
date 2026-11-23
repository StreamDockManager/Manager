"""Native binary-only Blacksmith build helpers; never publish or install."""
import argparse
import io
from pathlib import Path
import platform
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {
    'linux-amd64': ('Linux', 'x86_64', 'x86_64-unknown-linux-gnu'),
    'linux-arm64': ('Linux', 'aarch64', 'aarch64-unknown-linux-gnu'),
    'windows-amd64': ('Windows', 'x86_64', 'x86_64-pc-windows-msvc'),
    'macos-arm64': ('Darwin', 'aarch64', 'aarch64-apple-darwin'),
}
PRODUCTS = {'desktop': 'stream-dock-manager', 'cli': 'stream-dock-manager-cli'}


def verify_host(target):
    system, arch, triple = TARGETS[target]
    actual_arch = {'amd64': 'x86_64', 'arm64': 'aarch64'}.get(platform.machine().lower(), platform.machine().lower())
    if (platform.system(), actual_arch) != (system, arch):
        raise ValueError(f'{target} requires native {system}/{arch}; refusing a different runner')
    rust = subprocess.check_output(['rustc', '-vV'], text=True)
    if f'host: {triple}' not in rust.splitlines():
        raise ValueError(f'{target} requires Rust host {triple}')


def archive(root, product, target):
    filename = PRODUCTS[product] + ('.exe' if target.startswith('windows-') else '')
    binary = root / 'target/release' / filename
    if not binary.is_file() or binary.is_symlink():
        raise ValueError(f'Missing regular selected binary: {filename}')
    output = root / 'artifacts/blacksmith' / f'{product}-{target}.tar.gz'
    output.parent.mkdir(parents=True, exist_ok=True)
    # Tar preserves executable permissions when downloaded through Actions artifacts.
    with output.open('xb') as stream, tarfile.open(fileobj=stream, mode='w:gz') as bundle:
        bundle.add(binary, arcname=filename, recursive=False)
        for name in ['LICENSE', 'THIRD_PARTY_NOTICES.md']:
            bundle.add(root / name, arcname=name, recursive=False)
        readme = (f'{product} / {target}: manually built native binary, not an installer or release.\n'
                  'Extract the archive before running. No installation, device access, signing,\n'
                  'notarization, hardware tests, or release qualification was performed.\n'
                  'Linux builds use Ubuntu 24.04 and need the matching shared OS libraries.\n'
                  'The desktop needs WebKitGTK on Linux and WebView2 on Windows.\n'
                  'This raw desktop binary has no application-bundle, udev, file-association,\n'
                  'or launch-service installation. Use normal installers for that integration.\n').encode()
        info = tarfile.TarInfo('BUILD-README.txt')
        info.size = len(readme)
        info.mode = 0o644
        bundle.addfile(info, io.BytesIO(readme))
    print(output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--product', choices=PRODUCTS, required=True)
    parser.add_argument('--target', choices=TARGETS, required=True)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument('--verify-host', action='store_true')
    mode.add_argument('--archive', action='store_true')
    args = parser.parse_args()
    if args.verify_host:
        verify_host(args.target)
    else:
        archive(ROOT, args.product, args.target)


if __name__ == '__main__':
    main()
