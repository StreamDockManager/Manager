#!/usr/bin/env python3
"""Add macOS install instructions to a new DMG without altering its app or layout."""
import argparse
from contextlib import contextmanager
import hashlib
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile

from verify_desktop_bundle import contained_path, regular_file

ROOT = Path(__file__).resolve().parent.parent
INSTRUCTIONS = 'INSTALL-INSTRUCTIONS.md'


# Run the selected command and propagate failure to the calling workflow
def run(arguments):
    subprocess.run(arguments, check=True, timeout=180)


# Mount the disk image for inspection and release the mount afterward
@contextmanager
def mounted(image, point, *, writable=False):
    point.mkdir()
    run(['hdiutil', 'attach', '-readwrite' if writable else '-readonly', '-nobrowse',
         '-mountpoint', str(point), str(image)])
    try:
        yield point
    finally:
        # A failed detach is an error; do not replace the original while mounted
        run(['hdiutil', 'detach', str(point)])


# Record disk-image files and links while excluding only the volume event journal
def inventory(root):
    result = {}
    for parent, directories, files in os.walk(root, followlinks=False):
        # macOS updates its volume-level event journal when a writable image is
        # mounted. It is filesystem bookkeeping, not app or Finder layout data
        if Path(parent) == root and '.fseventsd' in directories and not (root / '.fseventsd').is_symlink():
            directories.remove('.fseventsd')
        for name in [*directories, *files]:
            path = Path(parent) / name
            relative = path.relative_to(root).as_posix()
            mode = path.lstat().st_mode
            if stat.S_ISLNK(mode):
                result[relative] = ('link', os.readlink(path))
            elif stat.S_ISREG(mode):
                with path.open('rb') as source:
                    digest = hashlib.file_digest(source, 'sha256').hexdigest()
                result[relative] = ('file', stat.S_IMODE(mode), path.stat().st_size, digest)
            elif not stat.S_ISDIR(mode):
                raise ValueError(f'Unexpected special file in DMG: {relative}')
    return result


# Finalize the desktop disk image and verify its packaged contents
def finalize(dmg, instructions):
    root = ROOT.resolve()
    dmg = regular_file(dmg, root)
    instructions = regular_file(instructions, root)
    contents = instructions.read_bytes()
    if not contents or dmg.suffix.lower() != '.dmg':
        raise ValueError('Expected a DMG and nonempty installation instructions')
    temporary = Path(tempfile.mkdtemp(prefix='.dmg-instructions-', dir=dmg.parent))
    try:
        with mounted(dmg, temporary / 'original') as volume:
            before = inventory(volume)
            existing = volume / INSTRUCTIONS
            if existing.exists() or existing.is_symlink():
                if existing.is_symlink() or not existing.is_file() or existing.read_bytes() != contents:
                    raise ValueError('Existing DMG instructions differ; refusing overwrite')
                return dmg
        writable = temporary / 'writable.dmg'
        run(['hdiutil', 'convert', str(dmg), '-format', 'UDRW', '-o', str(writable)])
        with mounted(writable, temporary / 'writable-volume', writable=True) as volume:
            target = volume / INSTRUCTIONS
            with target.open('xb') as destination:
                destination.write(contents)
            target.chmod(0o644)
        completed = temporary / 'completed.dmg'
        run(['hdiutil', 'convert', str(writable), '-format', 'UDZO', '-o', str(completed)])
        run(['hdiutil', 'verify', str(completed)])
        with mounted(completed, temporary / 'verified') as volume:
            target = regular_file(volume / INSTRUCTIONS, volume)
            if target.read_bytes() != contents:
                raise ValueError('DMG installation instructions failed verification')
            after = inventory(volume)
            after.pop(INSTRUCTIONS)
            if before != after:
                changed = sorted(name for name in before.keys() | after.keys() if before.get(name) != after.get(name))
                raise ValueError(f'Adding DMG instructions changed existing app or layout files: {changed[:20]}')
        # Native cataloguing happens after this atomic replacement. Any earlier
        # error leaves the original DMG byte-for-byte intact
        os.replace(completed, dmg)
    finally:
        # Never recursively remove an image's mounted contents after a failed
        # attach/detach. Keep the owned temporary directory for diagnosis
        points = [temporary / name for name in ('original', 'writable-volume', 'verified')]
        if not any(os.path.ismount(point) for point in points):
            shutil.rmtree(temporary)
    return dmg


# Parse options and run the finalize desktop dmg workflow
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--dir', type=Path, required=True)
    args = parser.parse_args()
    directory = contained_path(args.dir, ROOT.resolve())
    images = list(directory.glob('dmg/*.dmg'))
    if len(images) != 1:
        raise ValueError('Expected exactly one freshly built desktop DMG')
    print(finalize(images[0], ROOT / 'packaging/install-instructions/macos' / INSTRUCTIONS))


if __name__ == '__main__':
    main()
