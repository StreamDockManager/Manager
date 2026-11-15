#!/usr/bin/env python3
"""Build and execute every registered connection-contract conformance test.

The JSON contract is build-validated against its schema and independent support
registry. The registry selects exact Rust tests, never shell commands. These are
local disposable fixtures, not installed vendor software or physical hardware.
"""
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
HOST = ROOT / 'crates/plugin-host'


# Select and run each declared connection-contract test exactly once
def run_conformance():
    registry = json.loads((HOST / 'connection-implementations.v1.json').read_text())
    if not all(isinstance(registry.get(key), list) and registry[key]
               for key in ('contract_tests', 'capabilities', 'vendors')):
        raise ValueError('Missing or empty conformance declarations')
    declarations = [registry['contract_tests']] + [entry['tests'] for entry in registry['capabilities'] + registry['vendors']]
    if any(not isinstance(names, list) or not names or
           any(not isinstance(name, str) or not name or name.startswith('-') for name in names)
           for names in declarations):
        raise ValueError('Invalid or empty conformance selection')
    requested = set(registry['contract_tests'])
    for entry in registry['capabilities'] + registry['vendors']:
        if not entry['implementation'] or not entry['tests']:
            raise ValueError('Missing implementation or conformance tests')
        requested.update(entry['tests'])
    if not requested or any(not isinstance(name, str) or not name or name.startswith('-') for name in requested):
        raise ValueError('Invalid or empty conformance selection')
    build = subprocess.run(
        ['cargo', 'test', '--locked', '-p', 'stream-dock-manager-plugin-host', '--lib', '--no-run', '--message-format=json'],
        cwd=ROOT, check=True, text=True, stdout=subprocess.PIPE,
    )
    binaries = []
    for line in build.stdout.splitlines():
        message = json.loads(line)
        if (message.get('reason') == 'compiler-artifact'
                and message.get('target', {}).get('name') == 'stream_dock_manager_plugin_host'
                and message.get('profile', {}).get('test') is True
                and message.get('executable')):
            binaries.append(message['executable'])
    if len(binaries) != 1:
        raise ValueError('Expected exactly one compiled plugin-host test harness')
    binary = binaries[0]
    listing = subprocess.run([binary, '--list', '--format', 'terse'], cwd=ROOT, check=True,
                             text=True, stdout=subprocess.PIPE).stdout
    available = {line.removesuffix(': test') for line in listing.splitlines() if line.endswith(': test')}
    missing = requested - available
    if missing:
        raise ValueError('Registered conformance tests do not exist: ' + ', '.join(sorted(missing)))
    # Never turn an ignored hardware/local-installation probe into qualification
    ignored = subprocess.run([binary, '--list', '--ignored', '--format', 'terse'], cwd=ROOT,
                             check=True, text=True, stdout=subprocess.PIPE).stdout
    ignored_names = {line.removesuffix(': test') for line in ignored.splitlines() if line.endswith(': test')}
    if requested & ignored_names:
        raise ValueError('Conformance must use executable disposable tests, not ignored probes: '
                         + ', '.join(sorted(requested & ignored_names)))
    for name in sorted(requested):
        subprocess.run([binary, '--exact', name, '--nocapture'], cwd=ROOT, check=True)
    print(f'Connection contract: {len(registry["capabilities"])} capabilities, '
          f'{len(registry["vendors"])} vendor subsets, {len(requested)} conformance tests passed')


if __name__ == '__main__':
    try:
        subprocess.run([sys.executable, str(ROOT / 'scripts/prepare_patched_deps.py')], cwd=ROOT, check=True)
        run_conformance()
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error)) from error
