#!/usr/bin/env python3
"""Package the separately built CLI without adding resources to the desktop app."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import struct
import subprocess
import tempfile
import zipfile

from packaging_files import sha256_file, contained_without_symlinks

import release
import package_cli_native
import package_metadata

ROOT = Path(__file__).resolve().parent.parent
ZIP_TIMESTAMP = (1980, 1, 1, 0, 0, 0)


# Read the third-party notice text included with the CLI distribution
def metadata_notice():
    # Keep native installers and portable ZIPs bound to the same exact payload
    return '\n## Package metadata\n\n```json\n' + json.dumps(package_metadata.load(), indent=2) + '\n```\n'


# Check that a resolved path stays inside the permitted workspace
def inside_workspace(path):
    return contained_without_symlinks(path, ROOT.resolve(),
                                      outside_message='CLI package paths must be inside the workspace',
                                      symlink_message='CLI package paths cannot contain symlinks')


# Require a regular packaging input without unsafe filesystem links
def regular_input(path):
    path = inside_workspace(path)
    if not path.is_file() or path.stat().st_size == 0:
        raise ValueError(f'CLI package input must be a nonempty regular file: {path.name}')
    return path


# Check the CLI executable against the requested target architecture
def validate_binary(path, osname, arch):
    # Read only bounded executable headers; a filename is not architecture proof
    with path.open('rb') as stream:
        header = stream.read(65536)
    valid = False
    if osname == 'linux' and len(header) >= 20 and header[:5] == b'\x7fELF\x02':
        valid = header[5] == 1 and struct.unpack_from('<H', header, 18)[0] == 62 and arch == 'amd64'
    elif osname == 'macos' and len(header) >= 8 and header[:4] == b'\xcf\xfa\xed\xfe':
        valid = struct.unpack_from('<I', header, 4)[0] == {'amd64': 0x01000007, 'arm64': 0x0100000C}.get(arch)
    elif osname == 'windows' and len(header) >= 64 and header[:2] == b'MZ':
        offset = struct.unpack_from('<I', header, 60)[0]
        valid = (offset + 6 <= len(header) and header[offset:offset + 4] == b'PE\0\0'
                 and struct.unpack_from('<H', header, offset + 4)[0] == 0x8664 and arch == 'amd64')
    if not valid:
        raise ValueError(f'CLI executable does not match {osname}/{arch}')


# Derive the reproducible packaging timestamp from source metadata
def source_timestamp(commit):
    head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
    if head != commit:
        raise ValueError('CLI release commit differs from the checked-out source')
    status = subprocess.check_output(
        ['git', 'status', '--porcelain=v1', '--untracked-files=normal'], cwd=ROOT, text=True,
    ).splitlines()
    if status:
        # Include bounded status records, not diffs, for failures on native build runners
        paths = '\n'.join(status[:20])
        if len(status) > 20:
            paths += f'\n… and {len(status) - 20} more status entries'
        raise ValueError('CLI package source must be a clean checkout of the selected commit\n' + paths)
    epoch = subprocess.check_output(['git', 'show', '-s', '--format=%ct', commit], cwd=ROOT, text=True).strip()
    # This schema field records the source epoch for reproducible build metadata,
    # not the time a GitHub release is published
    return datetime.fromtimestamp(int(epoch), timezone.utc).isoformat()


# Calculate the file content digest used for integrity checks
def file_hash(path):
    return sha256_file(path)


# Read every stored byte before publishing the local artifact and its hash
def verify_archive(archive, entries, readme):
    """Read every stored byte before publishing the local artifact and its hash."""
    with zipfile.ZipFile(archive) as packaged:
        if packaged.namelist() != [name for name, _, _ in entries]:
            raise ValueError('CLI archive inventory differs from the expected files')
        for info, (_, source, mode) in zip(packaged.infolist(), entries):
            expected_size = len(readme) if source is None else source.stat().st_size
            expected_hash = hashlib.sha256(readme).hexdigest() if source is None else file_hash(source)
            if (info.file_size != expected_size or info.date_time != ZIP_TIMESTAMP
                    or info.create_system != 3 or info.external_attr >> 16 != (stat.S_IFREG | mode)):
                raise ValueError(f'CLI archive metadata differs: {info.filename}')
            # Streaming reads validate the ZIP CRC as well as the source SHA-256
            with packaged.open(info) as stream:
                if hashlib.file_digest(stream, 'sha256').hexdigest() != expected_hash:
                    raise ValueError(f'CLI archive bytes differ: {info.filename}')


# Collect the runtime files and notices for the CLI archive
def package_entries(binary, osname, arch):
    if (osname, arch, '.zip') not in release.CLI_VARIANTS:
        raise ValueError('Unsupported CLI platform')
    binary = regular_input(binary)
    validate_binary(binary, osname, arch)
    notices = [regular_input(ROOT / name) for name in ['LICENSE', 'THIRD_PARTY_NOTICES.md']]
    instructions = regular_input(ROOT / 'packaging/install-instructions' / osname / 'INSTALL-INSTRUCTIONS.md')
    executable = 'stream-dock-manager-cli.exe' if osname == 'windows' else 'stream-dock-manager-cli'
    return [(executable, binary, 0o755), *[(p.name, p, 0o644) for p in notices],
            ('INSTALL-INSTRUCTIONS.md', instructions, 0o644), ('README.md', None, 0o644)]


# Build native installers from the validated product payload
def build_installers(entries, readme, directory, osname, arch, version):
    payload, work = directory / 'payload', directory / 'work'
    payload.mkdir()
    work.mkdir()
    for filename, source, mode in entries:
        target = payload / filename
        if source is None:
            target.write_bytes(readme)
        else:
            shutil.copyfile(source, target)
        target.chmod(mode)
    rules = regular_input(ROOT / 'packaging/linux/60-stream-dock-manager.rules') if osname == 'linux' else None
    return package_cli_native.build_native(payload, work, directory, osname, arch, version, rules)


# Package the locally built CLI for its selected target
def package_local(binary, directory, osname, arch):
    entries = package_entries(binary, osname, arch)
    version = release.version()
    readme = (f'Stream Dock Manager CLI {version} — {osname}/{arch}\n\n'
              'This local build is unsigned. Read INSTALL-INSTRUCTIONS.md for setup.\n' + metadata_notice()).encode()
    directory = inside_workspace(directory)
    directory.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.cli-package-', dir=directory) as temporary:
        paths = build_installers(entries, readme, Path(temporary), osname, arch, version)
        for path in paths:
            # Publish completed installers without overwriting an existing build
            os.link(path, directory / path.name)


# Build and package the CLI using the selected target configuration
def package_cli(binary, directory, osname, arch, *, native=False):
    entries = package_entries(binary, osname, arch)
    directory = inside_workspace(directory)
    if directory.exists() and not directory.is_dir():
        raise ValueError('CLI package output must be a directory')
    repository = os.environ['GITHUB_REPOSITORY']
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repository) or any(part in ('.', '..') for part in repository.split('/')):
        raise ValueError('Invalid CLI release repository')
    commit, build = os.environ['RELEASE_COMMIT'], os.environ['GITHUB_RUN_ID']
    if not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', commit) or not re.fullmatch(r'[1-9][0-9]*', build):
        raise ValueError('Invalid CLI source commit/build identity')
    version = release.version()
    development = release.build_only()
    tag = '' if development else os.environ['RELEASE_TAG']
    if not development and tag != f'v{version}':
        raise ValueError('CLI release tag differs from VERSION')
    published_at = source_timestamp(commit)
    name = f'stream-dock-manager-cli-{version}-{osname}-{arch}.zip'
    catalog_name = f'cli-catalog-{osname}-{arch}.json'
    names = [name, *[f'stream-dock-manager-cli-{version}-{osname}-{arch}{extension}' for extension in package_cli_native.FORMATS[osname]]] if native else [name]
    for output_name in [*names, catalog_name]:
        path = inside_workspace(directory / output_name)
        if path.exists() and not path.is_file():
            raise ValueError('CLI package output cannot replace a non-file')
    executable = 'stream-dock-manager-cli.exe' if osname == 'windows' else 'stream-dock-manager-cli'
    source_url = f'https://github.com/{repository}/commit/{commit}'
    build_url = f'https://github.com/{repository}/actions/runs/{build}'
    readme = (
        f'Stream Dock Manager CLI {version} — {osname}/{arch}\n\n'
        f'{release.CATALOG_RELEASE_NOTES}\n\n'
        f'Extract this archive and run {executable} --help. Add the extracted directory to PATH if desired. '
        'The desktop app is distributed separately; this archive does not install or launch it. '
        'Quit Manager before using install-plugins to change its plugin installation. '
        'Linux device access requires the documented udev setup. MP4 input requires an external FFmpeg installation.\n\n'
        'Read INSTALL-INSTRUCTIONS.md for this operating system. Native installers and portable ZIPs are alternative distributions; no installer changes PATH automatically.\n\n'
        f'Source commit: {source_url}\nBuild run: {build_url}\n'
        f'Build workflow: https://github.com/{repository}/blob/{commit}/.github/workflows/build-cli.yml\n'
        'These links support inspection; they do not prove that this executable matches the source.\n' + metadata_notice()
    ).encode('utf-8')
    identity = {'schema_version': 1, 'product': release.CLI_PRODUCT, 'repository': repository, 'version': version,
                'channel': 'development' if development else 'stable', 'build': build, 'commit': commit,
                'published_at': published_at, 'release_notes': release.CATALOG_RELEASE_NOTES, 'release_tag': tag,
                'packages': []}
    # Select the matching asset from the validated dependency inventory
    def asset_for(path):
        return {'name': path.name, 'url': '' if development else f'https://github.com/{repository}/releases/download/{tag}/{path.name}',
                'sha256': file_hash(path), 'size': path.stat().st_size, 'os': osname, 'arch': arch, 'signing': 'unsigned'}
    directory.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.cli-package-', dir=directory) as temporary:
        temporary = Path(temporary)
        archive = temporary / name
        with zipfile.ZipFile(archive, 'w', compression=zipfile.ZIP_DEFLATED, compresslevel=6) as output:
            for filename, source, mode in entries:
                info = zipfile.ZipInfo(filename, ZIP_TIMESTAMP)
                info.create_system = 3
                info.external_attr = (stat.S_IFREG | mode) << 16
                info.compress_type = zipfile.ZIP_DEFLATED
                info._compresslevel = 6  # ZipInfo streaming writes do not inherit the archive level
                with output.open(info, 'w') as destination:
                    if source is None:
                        destination.write(readme)
                    else:
                        with source.open('rb') as stream:
                            shutil.copyfileobj(stream, destination, 65536)
        verify_archive(archive, entries, readme)
        if native and any((directory / filename).exists() for filename in [*names, catalog_name]):
            # Native tools may use random identifiers/timestamps. Reuse verified
            # original bytes; the reproducible ZIP binds every payload input
            if not all((directory / filename).is_file() for filename in [*names, catalog_name]):
                raise ValueError('Incomplete retained native CLI artifacts; recover the original complete output')
            saved = json.loads((directory / catalog_name).read_text(encoding='utf-8'))
            expected = {**identity, 'assets': [asset_for(directory / filename) for filename in names]}
            if saved != expected or file_hash(directory / name) != file_hash(archive):
                raise ValueError('Retained native CLI artifacts or inputs differ; refusing overwrite')
            return directory / name
        paths = [archive]
        if native:
            paths.extend(build_installers(entries, readme, temporary, osname, arch, version))
            if [path.name for path in paths] != names or any(path.parent != temporary for path in paths):
                raise ValueError('Native CLI outputs differ from the requested inventory')
        catalog = {**identity, 'assets': [asset_for(path) for path in paths]}
        (temporary / catalog_name).write_text(json.dumps(catalog, indent=4) + '\n', encoding='utf-8')
        # Check both destinations before replacing either; retries reuse exact bytes
        for output_name in [*names, catalog_name]:
            destination = directory / output_name
            if destination.exists() and file_hash(destination) != file_hash(temporary / output_name):
                raise ValueError(f'CLI package output differs: {output_name}; refusing overwrite')
        for output_name in [*names, catalog_name]:
            destination = directory / output_name
            if not destination.exists():
                os.replace(temporary / output_name, destination)
    return directory / name


# Parse options and run the package cli workflow
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--dir', type=Path, required=True)
    parser.add_argument('--os', required=True)
    parser.add_argument('--arch', required=True)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument('--native', action='store_true', help='Also build this OS\'s installers')
    modes.add_argument('--local', action='store_true', help='Build local installers without release metadata')
    args = parser.parse_args()
    if args.local:
        package_local(args.binary, args.dir, args.os, args.arch)
    else:
        print(package_cli(args.binary, args.dir, args.os, args.arch, native=args.native))


if __name__ == '__main__':
    main()
