#!/usr/bin/env python3
"""Verify macOS desktop resources and retain a size inventory beside the DMG."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import stat
import subprocess

from package_cli import validate_binary
from packaging_files import contained_without_symlinks

ROOT = Path(__file__).resolve().parent.parent


# Resolve a bundle path while rejecting escape from its root
def contained_path(path, root):
    return contained_without_symlinks(path, root,
                                      outside_message='Bundle verification paths must stay inside their root',
                                      symlink_message='Bundle verification inputs cannot use symlinks')


# Require a regular bundle file before integrity inspection
def regular_file(path, root):
    path = contained_path(path, root)
    if not path.is_file():
        raise ValueError(f'Missing regular bundle file: {path.name}')
    return path


# Require a valid local ad-hoc bundle seal, without publisher credentials
def verify_native_integrity(application):
    """Require a valid local ad-hoc bundle seal, without publisher credentials."""
    application = contained_path(application, ROOT.resolve())
    if platform.system() != 'Darwin':
        raise ValueError('Native macOS bundle integrity verification requires macOS')

    # Run the platform code-signature verification for the selected bundle
    def codesign(arguments):
        try:
            result = subprocess.run(['/usr/bin/codesign', *arguments, str(application)],
                                    capture_output=True, text=True, timeout=30)
        except (OSError, subprocess.TimeoutExpired) as error:
            raise ValueError(f'Cannot verify native macOS bundle integrity: {error}') from error
        if result.returncode:
            detail = (result.stderr or result.stdout).strip()[:2000]
            raise ValueError(f'Native macOS bundle integrity verification failed: {detail}')
        return (result.stdout + '\n' + result.stderr).splitlines()

    # A linker-signed Mach-O inside an unsealed .app fails this check even when
    # that executable runs locally. This is separate from Gatekeeper trust
    codesign(['--verify', '--deep', '--strict', '--verbose=4'])
    seal = regular_file(application / 'Contents/_CodeSignature/CodeResources', application)
    if not seal.stat().st_size:
        raise ValueError('Native macOS bundle resource seal is empty')
    lines = [line.strip() for line in codesign(['--display', '--verbose=4'])]
    signatures = [line for line in lines if line.startswith('Signature=')]
    if signatures != ['Signature=adhoc']:
        raise ValueError('Native macOS bundle must use only an ad-hoc signature')
    if any(line.startswith(('Authority=', 'Timestamp='))
           or (line.startswith('TeamIdentifier=') and line != 'TeamIdentifier=not set')
           or line.casefold().startswith('notarization') for line in lines):
        raise ValueError('Native macOS bundle must not contain a publisher or notarization identity')
    bound_info = [re.fullmatch(r'Info\.plist entries=([1-9]\d*)', line) for line in lines]
    sealed_resources = [re.fullmatch(r'Sealed Resources version=(\d+) rules=([1-9]\d*) files=([1-9]\d*)', line) for line in lines]
    if not any(bound_info) or not any(match and int(match[1]) >= 2 for match in sealed_resources):
        raise ValueError('Native macOS bundle must bind its Info.plist and resource seal')
    return {
        'strict_verification_passed': True, 'signature': 'adhoc',
        'publisher_identity_absent': True, 'notarization_identity_absent': True,
        'info_plist_bound': True, 'resource_seal_verified': True,
        'gatekeeper_acceptance': 'not_claimed',
    }


# Inspect the desktop bundle layout and required native contents
def inspect_bundle(directory, arch, *, repository, commit, build, native_integrity=False):
    root = ROOT.resolve()
    directory = contained_path(directory, root)
    applications = list(directory.glob('macos/*.app'))
    if len(applications) != 1:
        raise ValueError('Expected exactly one macOS application bundle')
    application = contained_path(applications[0], root)
    if not application.is_dir():
        raise ValueError('Expected an application directory')
    if (not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repository)
            or any(part in ('.', '..') for part in repository.split('/'))
            or not re.fullmatch(r'[0-9a-f]{40}|[0-9a-f]{64}', commit)
            or not re.fullmatch(r'[1-9][0-9]*', build)):
        raise ValueError('Invalid bundle source/build identity')
    if arch not in ('amd64', 'arm64'):
        raise ValueError('Unsupported macOS bundle architecture')
    total_bytes, file_count, symlink_count = 0, 0, 0
    for parent, directories, filenames in os.walk(application, followlinks=False):
        for name in [*directories, *filenames]:
            path = Path(parent) / name
            relative = path.relative_to(application).as_posix()
            if (name.casefold() in ('stream-dock-manager-cli', 'stream-dock-manager-cli.exe')
                    or relative.casefold() == 'contents/resources/cli'):
                raise ValueError(f'Desktop bundle contains the separately distributed CLI: {relative}')
            metadata = path.lstat()
            if stat.S_ISLNK(metadata.st_mode):
                # Framework aliases are valid only when their full link chain
                # reaches an existing entry inside this application bundle
                try:
                    target = path.resolve(strict=True)
                except (OSError, RuntimeError) as error:
                    raise ValueError(f'Bundle symlinks have missing or cyclic targets: {relative}') from error
                if not target.is_relative_to(application):
                    raise ValueError(f'Bundle symlinks cannot escape the application: {relative}')
                symlink_count += 1
            elif stat.S_ISREG(metadata.st_mode):
                file_count += 1
                total_bytes += metadata.st_size
            elif not stat.S_ISDIR(metadata.st_mode):
                raise ValueError(f'Unexpected special file in desktop bundle: {relative}')
    plist_path = regular_file(application / 'Contents/Info.plist', application)
    with plist_path.open('rb') as source:
        bundle_info = plistlib.load(source)
    executable = bundle_info.get('CFBundleExecutable')
    schemes = [
        scheme for url_type in bundle_info.get('CFBundleURLTypes', [])
        for scheme in url_type.get('CFBundleURLSchemes', [])
    ]
    if schemes.count('stream-dock-manager') != 1:
        raise ValueError('Manager bundle must register the catalog URL scheme exactly once')
    if (not isinstance(executable, str) or not executable or executable in ('.', '..')
            or '/' in executable or '\\' in executable or '\0' in executable):
        raise ValueError('Invalid application executable name')
    binary = regular_file(application / 'Contents/MacOS' / executable, application)
    validate_binary(binary, 'macos', arch)
    for name in ('LICENSE', 'THIRD_PARTY_NOTICES.md'):
        bundled = regular_file(application / 'Contents/Resources/notices' / name, application)
        original = regular_file(root / name, root)
        if bundled.read_bytes() != original.read_bytes():
            raise ValueError(f'Desktop bundle notice differs from source: {name}')
    instructions = regular_file(application / 'Contents/Resources/INSTALL-INSTRUCTIONS.md', application)
    source_instructions = regular_file(root / 'packaging/install-instructions/macos/INSTALL-INSTRUCTIONS.md', root)
    if instructions.read_bytes() != source_instructions.read_bytes():
        raise ValueError('Desktop bundle installation instructions differ from source')
    with binary.open('rb') as source:
        digest = hashlib.file_digest(source, 'sha256').hexdigest()
    report = {
        'schema_version': 1, 'os': 'macos', 'arch': arch, 'application': application.name,
        'repository': repository, 'commit': commit, 'build': build,
        'application_regular_file_bytes': total_bytes, 'regular_file_count': file_count,
        'symlink_count': symlink_count, 'main_executable': binary.relative_to(application).as_posix(),
        'main_executable_bytes': binary.stat().st_size, 'main_executable_sha256': digest,
        'cli_absent': True, 'source_notices_match': True, 'source_installation_instructions_match': True,
    }
    if native_integrity:
        report['native_integrity'] = verify_native_integrity(application)
    return report


# Parse options and run the verify desktop bundle workflow
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--dir', type=Path, required=True)
    parser.add_argument('--arch', required=True)
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    report = inspect_bundle(args.dir, args.arch, repository=os.environ['GITHUB_REPOSITORY'],
                            commit=os.environ['RELEASE_COMMIT'], build=os.environ['GITHUB_RUN_ID'],
                            native_integrity=True)
    destination = contained_path(args.report, ROOT.resolve())
    destination.write_text(json.dumps(report, indent=4) + '\n', encoding='utf-8')
    print(json.dumps(report, indent=2))


if __name__ == '__main__':
    main()
