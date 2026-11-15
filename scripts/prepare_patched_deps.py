#!/usr/bin/env python3
"""Prepare hash-pinned, locally patched Rust dependencies before Cargo resolution.

No global Cargo cache is modified. Source trees and archives are disposable;
only the manifest and patches are authoritative. No patch executable is needed.
"""
import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import sys
import tarfile
import tempfile
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
MAX_ARCHIVE = 32 * 1024 * 1024
MAX_SOURCE = 128 * 1024 * 1024


# Calculate the file content digest used for integrity checks
def sha256(path):
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(64 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


# Require the expected regular file before reading trusted input
def regular(path):
    return stat.S_ISREG(path.lstat().st_mode)


# Check whether a path contains a filesystem link
def linked(path):
    # Junctions can redirect directory writes on Windows just like symlinks
    return path.is_symlink() or getattr(path, 'is_junction', lambda: False)()


# Validate the directory before using it as an owned work location
def directory(path):
    if linked(path):
        raise ValueError(f'Refusing symbolic-link directory: {path}')
    path.mkdir(parents=True, exist_ok=True)
    if not path.is_dir():
        raise ValueError(f'Not a directory: {path}')


# Content identity independent of tar timestamps, host umask and platform
def tree_hash(root):
    """Content identity independent of tar timestamps, host umask and platform."""
    digest = hashlib.sha256()
    if linked(root) or not root.is_dir():
        raise ValueError('Prepared source is not a regular directory')
    for path in sorted(root.rglob('*'), key=lambda p: p.relative_to(root).as_posix()):
        relative = path.relative_to(root).as_posix()
        mode = path.lstat().st_mode
        if linked(path):
            raise ValueError(f'Unexpected non-regular source entry: {relative}')
        if stat.S_ISDIR(mode):
            continue
        if not stat.S_ISREG(mode):
            raise ValueError(f'Unexpected non-regular source entry: {relative}')
        digest.update(relative.encode('utf-8') + b'\0')
        digest.update(bytes.fromhex(sha256(path)))
    return digest.hexdigest()


# Validate archive path components before resolving the extraction target
def safe_parts(name):
    path = PurePosixPath(name)
    if (not name or '\\' in name or ':' in name or '\0' in name
            or path.is_absolute() or any(p in ('', '.', '..') for p in name.split('/'))):
        raise ValueError(f'Unsafe archive or patch path: {name}')
    return path.parts


# Extract only bounded regular files/directories, never links or devices
def extract(archive, destination, package):
    """Extract only bounded regular files/directories, never links or devices."""
    directory(destination)
    seen = set()
    total = 0
    count = 0
    with tarfile.open(archive, 'r:gz') as source:
        for member in source:
            count += 1
            if count > 20000:
                raise ValueError('Archive has too many entries')
            parts = safe_parts(member.name.rstrip('/'))
            if parts[0] != package:
                raise ValueError('Archive root does not match pinned package')
            key = '/'.join(parts).casefold()
            if key in seen:
                raise ValueError('Duplicate archive entry')
            seen.add(key)
            if not (member.isfile() or member.isdir()):
                raise ValueError('Archive links/devices are not permitted')
            if len(parts) == 1:
                if not member.isdir():
                    raise ValueError('Archive package root is not a directory')
                continue
            target = destination.joinpath(*parts[1:])
            if member.isdir():
                directory(target)
                continue
            total += member.size
            if member.size < 0 or total > MAX_SOURCE:
                raise ValueError('Expanded archive exceeds size limit')
            directory(target.parent)
            with source.extractfile(member) as input_file, target.open('xb') as output:
                shutil.copyfileobj(input_file, output, 64 * 1024)
            # Keep executability from the trusted archive; discard special bits
            target.chmod(0o755 if member.mode & 0o111 else 0o644)


# Strict unified text patches: exact context/offsets, no fuzzy matching
def apply_patch(root, patch, strip):
    """Strict unified text patches: exact context/offsets, no fuzzy matching."""
    lines = patch.read_bytes().decode('utf-8').splitlines(keepends=True)
    cursor = 0
    files = 0
    while cursor < len(lines):
        if not lines[cursor].startswith('--- '):
            raise ValueError(f'Unsupported patch header in {patch.name}')
        before = lines[cursor][4:].strip().split('\t')[0]
        cursor += 1
        if cursor == len(lines) or not lines[cursor].startswith('+++ '):
            raise ValueError('Missing patch target header')
        after = lines[cursor][4:].strip().split('\t')[0]
        cursor += 1
        old = safe_parts(before)[strip:]
        new = safe_parts(after)[strip:]
        if not old or old != new:
            raise ValueError('Patch rename/create/delete is unsupported')
        target = root.joinpath(*old)
        if not regular(target):
            raise ValueError('Patch target is not a regular source file')
        original = target.read_bytes().decode('utf-8').splitlines(keepends=True)
        output = []
        consumed = 0
        hunks = 0
        while cursor < len(lines) and lines[cursor].startswith('@@ '):
            match = re.fullmatch(r'@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@[^\n]*\n?', lines[cursor])
            if not match:
                raise ValueError('Invalid patch hunk')
            start, old_count, new_start, new_count = (int(match[1]), int(match[2] or 1), int(match[3]), int(match[4] or 1))
            index = start - 1 if old_count else start
            if index < consumed or index > len(original):
                raise ValueError('Patch hunk offset is outside source')
            output.extend(original[consumed:index])
            if len(output) != (new_start - 1 if new_count else new_start):
                raise ValueError('Patch new-line offset does not match source')
            consumed = index
            cursor += 1
            removed = added = 0
            while removed < old_count or added < new_count:
                if cursor >= len(lines) or lines[cursor][:1] not in (' ', '+', '-'):
                    raise ValueError('Truncated patch hunk')
                tag, text = lines[cursor][0], lines[cursor][1:]
                cursor += 1
                if tag in (' ', '-'):
                    if consumed >= len(original) or original[consumed] != text:
                        raise ValueError(f'Patch context mismatch in {patch.name}: {target.name}')
                    consumed += 1
                    removed += 1
                if tag in (' ', '+'):
                    output.append(text)
                    added += 1
                if removed > old_count or added > new_count:
                    raise ValueError('Patch line counts do not match hunk')
            hunks += 1
        if not hunks:
            raise ValueError('Patch has no hunks')
        output.extend(original[consumed:])
        target.write_bytes(''.join(output).encode('utf-8'))
        files += 1
    if not files:
        raise ValueError('Patch has no files')


# Hold the dependency-preparation lock for the protected operation
@contextmanager
def locked(cache):
    directory(cache)
    path = cache / '.prepare.lock'
    if linked(path):
        raise ValueError('Preparation lock must not be a link')
    flags = os.O_RDWR | os.O_CREAT | getattr(os, 'O_NOFOLLOW', 0)
    fd = os.open(path, flags, 0o600)
    try:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise ValueError('Preparation lock is not a regular file')
        os.write(fd, b'0')
        deadline = time.monotonic() + 120
        while True:
            try:
                if os.name == 'nt':
                    import msvcrt
                    os.lseek(fd, 0, os.SEEK_SET)
                    msvcrt.locking(fd, msvcrt.LK_NBLCK, 1)
                else:
                    import fcntl
                    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except OSError:
                if time.monotonic() > deadline:
                    raise TimeoutError('Another dependency preparation is still running')
                time.sleep(0.1)
        yield
    finally:
        os.close(fd)  # OS releases the lock, including after interrupted runs


class PinnedRedirects(urllib.request.HTTPRedirectHandler):
    # Apply the download redirect policy before forwarding the request
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if newurl != req.full_url:
            raise ValueError('Pinned crate download unexpectedly redirected')
        return super().redirect_request(req, fp, code, msg, headers, newurl)


# Resolve or download the checksum-verified dependency archive
def archive_for(cache, dependency, offline):
    name = f"{dependency['name']}-{dependency['version']}"
    directory(cache / 'archives')
    archive = cache / 'archives' / f'{name}.crate'
    if archive.exists() and regular(archive) and archive.stat().st_size <= MAX_ARCHIVE and sha256(archive) == dependency['sha256']:
        return archive
    if linked(archive):
        raise ValueError('Refusing symbolic-link archive cache')
    if offline:
        raise ValueError(f'Offline cache missing or corrupt: {archive.name}. Run preparation online once.')
    url = f"https://static.crates.io/crates/{dependency['name']}/{name}.crate"
    opener = urllib.request.build_opener(PinnedRedirects())
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(prefix='.download-', dir=archive.parent, delete=False) as output:
            temporary = Path(output.name)
            with opener.open(url, timeout=60) as response:
                if response.geturl() != url:
                    raise ValueError('Pinned crate URL changed')
                count = 0
                while chunk := response.read(64 * 1024):
                    count += len(chunk)
                    if count > MAX_ARCHIVE:
                        raise ValueError('Crate archive exceeds download limit')
                    output.write(chunk)
        if sha256(temporary) != dependency['sha256']:
            raise ValueError(f'Archive checksum mismatch: {name}')
        os.replace(temporary, archive)
        return archive
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


# Verify and prepare patched dependency trees under the shared preparation lock
def prepare(root=ROOT, offline=False, check=False):
    if linked(root / 'patches') or linked(root / 'vendor'):
        raise ValueError('Dependency metadata directories must not be links')
    manifest = json.loads((root / 'vendor/dependencies.json').read_text())
    if manifest.get('version') != 1:
        raise ValueError('Unsupported patched dependency manifest')
    dependencies = manifest['dependencies']
    if not isinstance(dependencies, list) or not dependencies:
        raise ValueError('Patched dependency inventory is empty')
    recipes = []
    for dependency in dependencies:
        if not re.fullmatch(r'[a-z0-9][a-z0-9_-]*', dependency['name']) or not re.fullmatch(r'\d+\.\d+\.\d+', dependency['version']):
            raise ValueError('Invalid pinned crate name/version')
        for field in ('sha256', 'tree_sha256'):
            if not re.fullmatch(r'[0-9a-f]{64}', dependency[field]):
                raise ValueError(f'Invalid {field}')
        patches = []
        for patch in dependency['patches']:
            parts = safe_parts(patch['file'])
            if len(parts) != 2 or parts[0] != 'patches' or not parts[1].endswith('.patch'):
                raise ValueError('Patches must be top-level patches/*.patch files')
            path = root.joinpath(*parts)
            if not regular(path) or sha256(path) != patch['sha256']:
                raise ValueError(f'Patch checksum mismatch: {patch["file"]}')
            if type(patch['strip']) is not int or not 0 <= patch['strip'] <= 4:
                raise ValueError('Invalid patch strip level')
            patches.append((path, patch['strip']))
        recipes.append((dependency, patches))
    cache = root / 'vendor/.cache'
    directory(root / 'vendor')
    with locked(cache):
        directory(cache / 'sources')
        for dependency, patches in recipes:
            name = f"{dependency['name']}-{dependency['version']}"
            target = cache / 'sources' / name
            if linked(target):
                raise ValueError('Refusing symbolic-link prepared source')
            if target.exists() and tree_hash(target) == dependency['tree_sha256']:
                print(f'Verified patched dependency: {name}')
                continue
            if check:
                raise ValueError(f'Prepared dependency missing or modified: {name}. Run scripts/prepare_patched_deps.py')
            archive = archive_for(cache, dependency, offline)
            with tempfile.TemporaryDirectory(prefix='.stage-', dir=cache) as temporary:
                staged = Path(temporary) / name
                extract(archive, staged, name)
                for patch, strip in patches:
                    apply_patch(staged, patch, strip)
                if tree_hash(staged) != dependency['tree_sha256']:
                    raise ValueError(f'Patched source checksum mismatch: {name}')
                old = Path(temporary) / 'previous'
                if target.exists():
                    os.replace(target, old)
                try:
                    os.replace(staged, target)
                except BaseException:
                    if old.exists():
                        os.replace(old, target)
                    raise
            print(f'Prepared patched dependency: {name}')


# Parse options and run the prepare patched deps workflow
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--offline', action='store_true', help='Never download; verified prepared trees or cached archives are required')
    parser.add_argument('--check', action='store_true', help='Verify prepared source without downloading or rebuilding it')
    args = parser.parse_args()
    try:
        prepare(offline=args.offline or os.environ.get('CARGO_NET_OFFLINE', '').lower() in ('true', '1'), check=args.check)
    except (OSError, ValueError, KeyError, tarfile.TarError, TimeoutError) as error:
        print(f'Patched dependency preparation failed: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
