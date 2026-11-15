#!/usr/bin/env python3
"""Provision checksum-pinned portable Windows CLI installer tools in the workspace."""
import argparse
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import tempfile
import urllib.parse
import urllib.request
import zipfile

from packaging_files import sha256_file

from verify_desktop_bundle import contained_path, regular_file

ROOT = Path(__file__).resolve().parent.parent
# NSIS SHA-256 is the official release-asset digest
TOOLS = {
    'nsis': {
        'url': 'https://github.com/tauri-apps/binary-releases/releases/download/nsis-3.13/nsis-3.13.zip',
        'sha256': 'ba63dffc4410ee89193e1cb5a41989991bd77c61068da17e3156d136b7b0b3d8',
        'size': 2363925,
        'executables': {'STREAM_DOCK_MANAGER_NSIS_MAKENSIS': 'nsis-3.13/makensis.exe'},
    },
}


# Calculate the file content digest used for integrity checks
def digest(path):
    return sha256_file(path)


# Check the installer-tool archive against its expected size and digest
def validate_archive(path, tool):
    if path.stat().st_size != tool['size'] or digest(path) != tool['sha256']:
        raise ValueError(f'Installer tool checksum/size mismatch: {path.name}')


# Reuse only an installer-tool archive with the expected integrity digest
def cached_archive(tool, directory):
    archive = contained_path(directory / (tool['sha256'] + '.zip'), ROOT.resolve())
    if archive.exists():
        validate_archive(regular_file(archive, ROOT.resolve()), tool)
        return archive
    with tempfile.NamedTemporaryFile(prefix='.download-', dir=directory, delete=False) as output:
        temporary = Path(output.name)
        try:
            with urllib.request.urlopen(tool['url'], timeout=60) as response:
                if urllib.parse.urlparse(response.url).scheme != 'https':
                    raise ValueError('Installer tools must download over HTTPS')
                total = 0
                while chunk := response.read(65536):
                    total += len(chunk)
                    if total > tool['size']:
                        raise ValueError('Installer tool exceeds its pinned download size')
                    output.write(chunk)
            output.flush()
            validate_archive(temporary, tool)
        except BaseException:
            output.close()
            temporary.unlink(missing_ok=True)
            raise
    os.replace(temporary, archive)
    return archive


# Extract validated installer-tool files into a controlled directory
def extract_archive(archive, destination, executables):
    # Check all entries before extracting any. The checksum is the trust anchor
    # these bounds also prevent unsafe filesystem behavior from a malformed ZIP
    with zipfile.ZipFile(archive) as source:
        entries = source.infolist()
        if len(entries) > 10000 or sum(info.file_size for info in entries) > 512 * 1024 * 1024:
            raise ValueError('Installer tool archive exceeds extraction limits')
        names = set()
        for info in entries:
            path = PurePosixPath(info.filename)
            mode = info.external_attr >> 16
            name = info.filename.casefold()
            # ZipInfo normalizes Windows separators and truncates NUL bytes
            # Validate the original archive name before using its normalized path
            if (info.orig_filename != info.filename or not info.filename
                    or '\\' in info.filename or ':' in info.filename or path.is_absolute()
                    or '..' in path.parts or name in names or stat.S_ISLNK(mode)
                    or (stat.S_IFMT(mode) not in (0, stat.S_IFREG, stat.S_IFDIR))):
                raise ValueError('Unsafe installer tool archive path or file type')
            names.add(name)
        for info in entries:
            target = destination / info.filename
            if info.is_dir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with source.open(info) as entry, target.open('xb') as output:
                    shutil.copyfileobj(entry, output, 65536)
    result = {}
    for variable, filename in executables.items():
        # Select the pinned tool's documented entry point. NSIS also ships a
        # Bin/makensis.exe, so a basename search is intentionally insufficient
        expected = regular_file(destination / filename, destination)
        result[variable] = str(expected.resolve())
    return result


# Provision the verified native installer tool without changing global state
def provision(directory):
    directory = contained_path(directory, ROOT.resolve())
    directory.mkdir(parents=True, exist_ok=True)
    output = {}
    for name, tool in TOOLS.items():
        archive = cached_archive(tool, directory)
        # Extract freshly each run so an edited executable in a previous cache
        # cannot bypass archive verification. Files stay in this owned cache
        destination = Path(tempfile.mkdtemp(prefix=f'{name}-', dir=directory))
        try:
            output.update(extract_archive(archive, destination, tool['executables']))
        except BaseException:
            shutil.rmtree(destination)
            raise
    return output


# Parse options and run the native installer tools workflow
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--dir', type=Path, default=ROOT / 'target/.installer-tools')
    args = parser.parse_args()
    output = provision(args.dir)
    if environment := os.environ.get('GITHUB_ENV'):
        with open(environment, 'a', encoding='utf-8') as destination:
            for name, value in output.items():
                if '\n' in value or '\r' in value:
                    raise ValueError('Installer tool path cannot contain a newline')
                destination.write(f'{name}={value}\n')
    print(json.dumps(output, indent=2))


if __name__ == '__main__':
    main()
