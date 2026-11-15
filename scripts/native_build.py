#!/usr/bin/env python3
"""Reject mislabeled native installer builds before compiling or cataloguing them."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import struct
import subprocess

from native_targets import NATIVE_TARGETS

LINUX_BASELINE_IMAGE = 'ubuntu@sha256:78d4744ead1f5b19b04d0df558ab182a5300903900755e515b2ca4d10eb2044d'

TARGETS = {(target.os, target.arch): (target.system, target.runner_arch, target.rust)
           for target in NATIVE_TARGETS}
SELECTIONS = {'all', 'linux-windows', 'linux', 'windows', 'macos-arm64', 'macos-amd64'}


# Require the actual host platform to match the requested native build
def validate_native_host(osname, arch, *, system, machine, runner_arch, rust_host, translated=False, cargo_target=''):
    expected_system, expected_runner, expected_rust = TARGETS[(osname, arch)]
    actual_arch = {'x86_64': 'amd64', 'amd64': 'amd64', 'arm64': 'arm64', 'aarch64': 'arm64'}.get(machine.lower())
    if system != expected_system or actual_arch != arch or runner_arch != expected_runner:
        raise ValueError(f'Native build requires {osname}/{arch}; actual host is {system}/{machine}, runner {runner_arch}')
    if translated:
        raise ValueError('Rosetta-translated runners cannot qualify native macOS installers; use a runner matching the physical architecture')
    if rust_host != expected_rust:
        raise ValueError(f'Native build requires Rust host {expected_rust}; got {rust_host}')
    if cargo_target:
        raise ValueError('Native build requires the default Cargo target directory layout; unset CARGO_BUILD_TARGET')


# Validate product and target combinations before invoking build tools
def validate_selection(mode, selection):
    if mode not in ('publish', 'build-only') or selection not in SELECTIONS:
        raise ValueError('Unknown native build mode or target selection')
    if mode == 'publish' and selection != 'all':
        raise ValueError('Publication requires all native installer targets; subsets are build-only')


# Check that the Linux build environment matches the packaging baseline
def validate_linux_userspace(distribution, version, libc, libc_version, image, container):
    if (distribution, version, libc, libc_version) != ('ubuntu', '24.04', 'glibc', '2.39'):
        raise ValueError('Linux builds require Ubuntu 24.04 userspace with glibc 2.39')
    if image != LINUX_BASELINE_IMAGE or not container:
        raise ValueError('Linux builds require the pinned Ubuntu 24.04 job container')


# Reject binaries requiring glibc versions above the supported baseline
def validate_glibc_requirements(output):
    names = sorted(set(re.findall(r'\bName: (GLIBC_[^\s]+)', output)))
    if not names or any(not re.fullmatch(r'GLIBC_\d+(?:\.\d+)+', name) for name in names):
        raise ValueError('ELF must declare numeric GLIBC version requirements')
    if any(tuple(map(int, name.removeprefix('GLIBC_').split('.'))) > (2, 39) for name in names):
        raise ValueError('ELF requires GLIBC newer than the Ubuntu 24.04 baseline (2.39)')
    return names


# Inspect ELF architecture and imported symbol requirements
def inspect_linux_binary(path):
    payload = path.read_bytes()
    if payload[:6] != b'\x7fELF\x02\x01' or len(payload) < 20 or struct.unpack_from('<H', payload, 18)[0] != 62:
        raise ValueError('Expected an ELF64 little-endian x86-64 executable')
    digest = hashlib.sha256(payload).hexdigest()
    output = subprocess.check_output(['readelf', '--version-info', '--wide', str(path)], text=True,
                                     env={**os.environ, 'LC_ALL': 'C'})
    requirements = validate_glibc_requirements(output)
    if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
        raise ValueError('ELF changed during GLIBC inspection')
    return dict(binary=str(path), binary_sha256=digest, binary_size=len(payload),
                elf_glibc_requirements=requirements)


# Parse options and run the native build workflow
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--os', choices=['linux', 'windows', 'macos'], required=True)
    parser.add_argument('--arch', choices=['amd64', 'arm64'], required=True)
    parser.add_argument('--mode', required=True)
    parser.add_argument('--targets', required=True)
    parser.add_argument('--binary', type=Path, help='Also verify the produced Linux ELF GLIBC floor')
    parser.add_argument('--report', type=Path, help='Retain successful build-environment evidence')
    args = parser.parse_args()
    validate_selection(args.mode, args.targets)
    rust = subprocess.check_output(['rustc', '-Vv'], text=True)
    rust_host = next((line.removeprefix('host: ') for line in rust.splitlines() if line.startswith('host: ')), '')
    translated = False
    if platform.system() == 'Darwin':
        result = subprocess.run(['sysctl', '-in', 'sysctl.proc_translated'], capture_output=True, text=True, check=False)
        # Intel macOS does not expose this key. A translated process returns 1
        translated = result.stdout.strip() == '1'
    report = dict(system=platform.system(), machine=platform.machine(), runner_arch=os.environ.get('RUNNER_ARCH', ''),
                  rust_host=rust_host, translated=translated, cargo_target=os.environ.get('CARGO_BUILD_TARGET', ''))
    validate_native_host(args.os, args.arch, **report)
    if args.os == 'linux':
        distribution = platform.freedesktop_os_release()
        libc, libc_version = platform.libc_ver()
        userspace = dict(distribution=distribution.get('ID', ''), version=distribution.get('VERSION_ID', ''),
                         libc=libc, libc_version=libc_version,
                         image=os.environ.get('STREAM_DOCK_MANAGER_LINUX_BASELINE_IMAGE', ''),
                         container=Path('/.dockerenv').is_file())
        print(json.dumps(userspace, indent=2), flush=True)
        validate_linux_userspace(**userspace)
        report['linux_userspace'] = userspace
        report['shared_host_kernel'] = platform.release()
        report['scope'] = 'Container userspace/build evidence; no desktop session, HID or host-distribution qualification'
    if args.binary:
        if args.os != 'linux':
            raise ValueError('--binary GLIBC inspection is Linux-only')
        report.update(inspect_linux_binary(args.binary))
    report['source_commit'] = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
    report['workflow_commit'] = os.environ.get('GITHUB_SHA', '')
    print(json.dumps(report, indent=2), flush=True)
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()
