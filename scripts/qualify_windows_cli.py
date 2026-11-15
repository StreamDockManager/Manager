#!/usr/bin/env python3
"""Bounded, opt-in native Windows CLI installer qualification.

Run on an otherwise idle, non-elevated Windows x64 account with no CLI installed.
Inputs are receipts produced by installer_inputs.py. The EXE lifecycle is:
older install -> same-version replacement -> uninstall -> reinstall -> uninstall.
Only --version, --help and --config <owned fixture> models execute the CLI.
No desktop, Runtime, device, PATH, protection or existing configuration changes
are requested. Uncertain partial installs/timeouts are retained for inspection;
this script leaves uncertain installer processes and installations for inspection.
--inspect-only records current installer state without running
installers, CLI commands, cleanup, or creating any configuration marker.

Installer execution requires Windows; pure verification tests run
on every host. A passing pure test suite is not native lifecycle qualification.
"""
import argparse
import ctypes
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import stat
import subprocess
import sys
import time
import uuid
import zipfile

import installer_inputs
from package_cli_native import REGISTRY_KEY, UNINSTALL_KEY

FILES = {'stream-dock-manager-cli.exe', 'LICENSE', 'THIRD_PARTY_NOTICES.md',
         'INSTALL-INSTRUCTIONS.md', 'README.md'}
GUID = re.compile(r'\{[0-9A-F]{8}(?:-[0-9A-F]{4}){3}-[0-9A-F]{12}\}')
# Detect pre-existing CLI MSI registrations without running a legacy installer
LEGACY_UPGRADE = '{' + str(uuid.uuid5(uuid.NAMESPACE_DNS, 'app.streamdockmanager.cli.msi')).upper() + '}'


# Reject a failed qualification precondition with its diagnostic message
def require(condition, message):
    if not condition:
        raise ValueError(message)


# Calculate the file content digest used for integrity checks
def digest(path):
    return installer_inputs.digest(path)


# Reject link-based paths in the qualification input tree
def no_links(path):
    path = Path(path).absolute()
    require('..' not in path.parts, 'Paths cannot contain parent traversal')
    require(not any(installer_inputs.linked(p) for p in (path, *path.parents)),
            'Path traverses a symbolic link or junction')
    return path


# Read-only, bounded manifest; fail rather than follow unknown links
def inventory(path):
    """Read-only, bounded manifest; fail rather than follow unknown links."""
    path = no_links(path)
    if not path.exists():
        return None
    require(path.is_dir(), 'Expected a directory')
    result, total = {}, 0
    for child in path.rglob('*'):
        no_links(child)
        name = child.relative_to(path).as_posix()
        require(len(result) < 100000, 'Configuration inventory exceeds file limit')
        if child.is_dir():
            result[name + '/'] = {'directory': True}
        else:
            require(stat.S_ISREG(child.stat().st_mode), 'Non-regular inventory member')
            size = child.stat().st_size
            total += size
            require(total <= 2 * 1024 ** 3, 'Configuration inventory exceeds byte limit')
            result[name] = {'size': size, 'sha256': digest(child)}
    return result


# Record the protected paths that must survive installer operations
def preservation_receipt(value):
    # Compare complete manifests in memory, but do not upload user profile names,
    # per-file hashes or the user's PATH as qualification artifacts
    return {'sha256': hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest(),
            'manager_entries': {name: None if value.get(name) is None else len(value[name])
                                for name in ('manager_roaming', 'manager_local')}}


# An absent real config gets one inert marker; existing config is read-only
def create_config_marker(config):
    """An absent real config gets one inert marker; existing config is read-only."""
    config = no_links(config)
    if config.exists():
        require(config.is_dir(), 'Manager configuration is not a directory')
        return None
    missing, parent = [], config
    while not parent.exists():
        missing.append(parent)
        parent = parent.parent
    require(parent.is_dir(), 'Manager configuration parent is not a directory')
    created = []
    for directory in reversed(missing):
        no_links(directory)
        directory.mkdir()  # No exist_ok: concurrent creation revokes ownership
        info = directory.stat()
        created.append({'path': directory, 'identity': (info.st_dev, info.st_ino)})
    marker = config / ('installer-qualification-' + uuid.uuid4().hex + '.sentinel')
    with marker.open('xb') as stream:
        stream.write(b'Owned native installer retention fixture; never load a Runtime.\n')
    info = marker.stat()
    return {'path': marker, 'sha256': digest(marker), 'identity': (info.st_dev, info.st_ino),
            'created': created}


# Remove only the owned configuration marker after verification
def remove_config_marker(marker):
    if marker is None:
        return
    path = no_links(marker['path'])
    info = path.stat()
    require((info.st_dev, info.st_ino) == marker['identity'] and digest(path) == marker['sha256'],
            'Real config marker changed; ownership cleanup refused')
    path.unlink()
    for entry in reversed(marker['created']):
        path = no_links(entry['path'])
        info = path.stat()
        require((info.st_dev, info.st_ino) == entry['identity'], 'Created config parent changed')
        path.rmdir()  # Never recursive; any new unrelated file prevents removal


# Record exact source text for installer evidence and preservation checks
def source_text_receipt(actual, source):
    expected = source.read_bytes()
    if actual == expected:
        transform = 'exact-git-bytes'
    elif b'\r' not in expected and actual == expected.replace(b'\n', b'\r\n'):
        transform = 'git-lf-to-windows-crlf'
    else:
        raise ValueError(f'ZIP differs from source text: {source.name}')
    return {'transform': transform, 'git_sha256': hashlib.sha256(expected).hexdigest(),
            'packaged_sha256': hashlib.sha256(actual).hexdigest(),
            'git_size': len(expected), 'packaged_size': len(actual)}


# Inspect the CLI ZIP payload without executing its contents
def zip_payload(inputs, *, document_report=None):
    with zipfile.ZipFile(inputs['assets']['.zip']) as archive:
        members = installer_inputs.safe_members(archive)
        require(len(members) == 5 and {m.filename for m in members} == FILES
                and all(not m.is_dir() for m in members), 'CLI ZIP must contain exactly five files')
        payload = {m.filename: archive.read(m) for m in members}
    for name in ('LICENSE', 'THIRD_PARTY_NOTICES.md', 'INSTALL-INSTRUCTIONS.md'):
        source = inputs['source_files'][name]
        receipt = source_text_receipt(payload[name], source)
        if document_report is not None:
            document_report[name] = receipt
    catalog = inputs['catalog']
    readme = payload['README.md'].decode('utf-8')
    for label, url in (
        ('Source commit', f"https://github.com/{catalog['repository']}/commit/{catalog['commit']}"),
        ('Build run', f"https://github.com/{catalog['repository']}/actions/runs/{catalog['build']}"),
        ('Build workflow', f"https://github.com/{catalog['repository']}/blob/{catalog['commit']}/.github/workflows/build-cli.yml"),
    ):
        require([line for line in readme.splitlines() if line.startswith(label + ':')] == [f'{label}: {url}'],
                'ZIP README source/build identity differs')
    binary = payload['stream-dock-manager-cli.exe']
    require(len(binary) >= 64 and binary[:2] == b'MZ', 'CLI ZIP lacks a PE executable')
    offset = int.from_bytes(binary[60:64], 'little')
    require(offset + 6 <= len(binary) and binary[offset:offset + 6] == b'PE\0\0\x64\x86',
            'CLI ZIP executable is not PE x86-64')
    return {name: {'size': len(data), 'sha256': hashlib.sha256(data).hexdigest()}
            for name, data in payload.items()}


# Require the selected CLI installation to be absent
def validate_absent(state):
    require(state['tree'] is None and not state['related'] and not state['registry'],
            'CLI destination, format/uninstall registry or related MSI product already exists')
    require(not state['processes'], 'A Manager/CLI process is running')


# Verify the installed CLI files and registration metadata
def validate_installed(state, bundle, target):
    require(not state['processes'], 'A Manager/CLI process is running')
    expected = bundle['payload']
    actual = state['tree']
    require(isinstance(actual, dict) and set(actual) == FILES | {'Uninstall.exe'},
            'Installed payload inventory differs')
    require(all(actual.get(name) == record for name, record in expected.items()),
            'Installed payload bytes differ from verified ZIP')
    registry = state['registry']
    format_key = 'HKCU64:format'
    require(registry.get(format_key) == {'InstallerKind': 'nsis'},
            'Installed per-user format marker differs')
    require(not state['related'], 'Unexpected related MSI product')
    require(set(registry) == {format_key, 'HKCU64:nsis'}, 'Unexpected installer registry ownership')
    entry = registry['HKCU64:nsis']
    require(entry.get('DisplayName') == 'Stream Dock Manager CLI' and entry.get('DisplayVersion') == bundle['inputs']['catalog']['version']
            and os.path.normcase(entry.get('InstallLocation', '')) == os.path.normcase(str(target))
            and entry.get('UninstallString') == f'"{target / "Uninstall.exe"}"'
            and entry.get('NoModify') == 1 and entry.get('NoRepair') == 1,
            'NSIS uninstall registration differs')
    require(actual['Uninstall.exe']['size'] > 0, 'Empty NSIS uninstaller')


class NativeWindows:
    # Require non-elevated Windows x64 and initialize native installer inspection APIs
    def __init__(self, report):
        require(os.name == 'nt' and platform.machine().upper() == 'AMD64'
                and ctypes.sizeof(ctypes.c_void_p) == 8, 'Native Windows x64 Python is required')
        from ctypes import wintypes as w
        self.w = w
        self.report, self.sequence, self.pending = report, 0, False
        self.pending_logs = []
        self.kernel = ctypes.WinDLL('kernel32', use_last_error=True)
        shell = ctypes.WinDLL('shell32', use_last_error=True)
        shell.IsUserAnAdmin.restype = w.BOOL
        require(not shell.IsUserAnAdmin(), 'Run as a non-elevated user; no elevation is requested')
        self.kernel.CreateMutexW.argtypes = (ctypes.c_void_p, w.BOOL, w.LPCWSTR)
        self.kernel.CreateMutexW.restype = w.HANDLE
        self.kernel.CloseHandle.argtypes = (w.HANDLE,)
        self.kernel.CloseHandle.restype = w.BOOL
        ctypes.set_last_error(0)
        self.lock = self.kernel.CreateMutexW(None, False, 'Local\\stream-dock-manager-cliQualification')
        require(bool(self.lock), 'Cannot acquire qualification mutex')
        if ctypes.get_last_error() == 183:
            self.close()
            raise ValueError('Another CLI qualification owns this user session')
        shell.SHGetFolderPathW.argtypes = (w.HWND, ctypes.c_int, w.HANDLE, w.DWORD, w.LPWSTR)
        shell.SHGetFolderPathW.restype = ctypes.c_long
        # Resolve the qualification folder used by this fixture
        def folder(identifier):
            value = ctypes.create_unicode_buffer(32768)
            require(shell.SHGetFolderPathW(None, identifier, None, 0, value) == 0, 'Known folder lookup failed')
            return no_links(value.value)
        self.local, self.roaming = folder(28), folder(26)
        self.target = self.local / 'Programs' / 'stream-dock-manager-cli'
        no_links(self.target)
        require(not any(c in str(self.target) for c in '\r\n"'), 'Unsupported native installation path')
        value = ctypes.create_unicode_buffer(32768)
        self.kernel.GetSystemDirectoryW.argtypes = (w.LPWSTR, w.UINT)
        self.kernel.GetSystemDirectoryW.restype = w.UINT
        require(0 < self.kernel.GetSystemDirectoryW(value, len(value)) < len(value), 'System directory lookup failed')
        system = Path(value.value)
        self.msi = ctypes.WinDLL(str(system / 'msi.dll'), use_last_error=True)
        self.msi.MsiEnumRelatedProductsW.argtypes = (w.LPCWSTR, w.DWORD, w.DWORD, w.LPWSTR)
        self.msi.MsiEnumRelatedProductsW.restype = w.UINT
    # Release the qualification resources owned by this session
    def close(self):
        if getattr(self, 'lock', None):
            lock, self.lock = self.lock, None
            require(bool(self.kernel.CloseHandle(lock)), 'Closing qualification mutex failed')

    # Run the selected command and collect its result
    def command(self, args, label, *, installer=False):
        self.sequence += 1
        stem = self.report / f'{self.sequence:02d}-{label}'
        record = {'command': [str(a) for a in args] if not isinstance(args, str) else args,
                  'installer': installer, 'started_at': datetime.now(timezone.utc).isoformat()}
        start = time.monotonic()
        with stem.with_suffix('.stdout.log').open('xb') as out, stem.with_suffix('.stderr.log').open('xb') as err:
            process = subprocess.Popen(args, stdin=subprocess.DEVNULL, stdout=out, stderr=err,
                                       cwd=self.report,
                                       # Background metadata commands must not flash a console
                                       creationflags=0 if installer else 0x08000000)
            record['pid'] = process.pid
            try:
                record['exit_code'] = process.wait(timeout=180 if installer else 30)
            except BaseException as error:
                record['timed_out'] = isinstance(error, subprocess.TimeoutExpired)
                record['interrupted_by'] = type(error).__name__
                if installer:
                    # Never race cleanup against an uncertain installation
                    self.pending = True
                    self.pending_logs = [stem.with_suffix('.stdout.log'), stem.with_suffix('.stderr.log')]
                else:
                    process.kill()
                    process.wait(timeout=10)
                raise ValueError(f'{label} interrupted ({type(error).__name__}); uncertain installer state is retained') from None
            finally:
                record['elapsed_seconds'] = time.monotonic() - start
                stem.with_suffix('.json').write_text(json.dumps(record, indent=2) + '\n')
        require(record['exit_code'] == 0, f'{label} exited {record["exit_code"]}; logs retained (reboot is not accepted)')
        return stem.with_suffix('.stdout.log').read_text(encoding='utf-8-sig')

    # Inspect the processes relevant to CLI installation and removal
    def processes(self):
        w = self.w
        class Entry(ctypes.Structure):
            _fields_ = [('dwSize', w.DWORD), ('cntUsage', w.DWORD), ('th32ProcessID', w.DWORD),
                        ('th32DefaultHeapID', ctypes.c_size_t), ('th32ModuleID', w.DWORD),
                        ('cntThreads', w.DWORD), ('th32ParentProcessID', w.DWORD),
                        ('pcPriClassBase', w.LONG), ('dwFlags', w.DWORD), ('szExeFile', w.WCHAR * 260)]
        self.kernel.CreateToolhelp32Snapshot.argtypes = (w.DWORD, w.DWORD)
        self.kernel.CreateToolhelp32Snapshot.restype = w.HANDLE
        handle = self.kernel.CreateToolhelp32Snapshot(2, 0)
        require(handle != ctypes.c_void_p(-1).value, 'Cannot enumerate running processes')
        found = []
        try:
            entry = Entry(dwSize=ctypes.sizeof(Entry))
            for name in ('Process32FirstW', 'Process32NextW'):
                fn = getattr(self.kernel, name)
                fn.argtypes, fn.restype = (w.HANDLE, ctypes.POINTER(Entry)), w.BOOL
            ok = self.kernel.Process32FirstW(handle, ctypes.byref(entry))
            while ok:
                if any(word in entry.szExeFile.casefold() for word in ('stream-dock-manager', 'vsd', 'mirabox')):
                    found.append({'pid': entry.th32ProcessID, 'name': entry.szExeFile})
                ok = self.kernel.Process32NextW(handle, ctypes.byref(entry))
            require(ctypes.get_last_error() == 18, 'Process enumeration failed')
        finally:
            require(bool(self.kernel.CloseHandle(handle)), 'Closing process snapshot failed')
        return found

    # Read installer-related Windows registry state
    def registry(self):
        import winreg
        found = {}
        for hive_name, hive in [('HKCU', winreg.HKEY_CURRENT_USER), ('HKLM', winreg.HKEY_LOCAL_MACHINE)]:
            for bits, flag in [(64, winreg.KEY_WOW64_64KEY), (32, winreg.KEY_WOW64_32KEY)]:
                for label, path in [('format', REGISTRY_KEY), ('nsis', UNINSTALL_KEY)]:
                    try:
                        key = winreg.OpenKey(hive, path, 0, winreg.KEY_READ | flag)
                    except FileNotFoundError:
                        continue
                    with key:
                        subkeys, values, _ = winreg.QueryInfoKey(key)
                        require(subkeys == 0, 'Unexpected subkey in installer registry')
                        data = {winreg.EnumValue(key, n)[0]: winreg.EnumValue(key, n)[1] for n in range(values)}
                        # HKCU\Software can be shared between registry views
                        previous = found.get(hive_name + '64:' + label)
                        if bits == 32 and previous == data:
                            continue
                        found[f'{hive_name}{bits}:{label}'] = data
        return found

    # Capture the current installer and filesystem state
    def state(self):
        related = []
        for index in range(100):
            value = ctypes.create_unicode_buffer(39)
            code = self.msi.MsiEnumRelatedProductsW(LEGACY_UPGRADE, 0, index, value)
            if code == 259:
                break
            require(code == 0 and GUID.fullmatch(value.value.upper()), 'MSI related-product enumeration failed')
            related.append(value.value.upper())
        else:
            raise ValueError('Too many related MSI products')
        return {'tree': inventory(self.target), 'related': sorted(related),
                'registry': self.registry(), 'processes': self.processes()}

    # Verify that protected files match their pre-operation receipts
    def preserved(self):
        import winreg
        paths = {}
        for label, hive, keypath in [('user', winreg.HKEY_CURRENT_USER, 'Environment'),
                                    ('machine', winreg.HKEY_LOCAL_MACHINE,
                                     r'SYSTEM\CurrentControlSet\Control\Session Manager\Environment')]:
            try:
                key = winreg.OpenKey(hive, keypath, 0, winreg.KEY_READ | winreg.KEY_WOW64_64KEY)
            except FileNotFoundError:
                paths[label] = None
                continue
            with key:
                try:
                    paths[label] = list(winreg.QueryValueEx(key, 'Path'))
                except FileNotFoundError:
                    paths[label] = None
        return {'manager_roaming': inventory(self.roaming / 'streamdockmanager' / 'Stream Dock Manager'),
                'manager_local': inventory(self.local / 'streamdockmanager' / 'Stream Dock Manager'), 'path': paths}

    # Resolve the owned configuration marker used by lifecycle checks
    def config_marker(self):
        return create_config_marker(self.roaming / 'streamdockmanager' / 'Stream Dock Manager' / 'config')

    # Run the selected installer within the qualification workflow
    def install(self, bundle, label):
        self.command([bundle['inputs']['assets']['.exe'], '/S'], label, installer=True)

    # Run the selected uninstaller within the qualification workflow
    def uninstall(self, label):
        # Run a verified copy so the installed uninstaller can be removed
        # NSIS requires _?= to be the final argument, without quotes
        source = self.target / 'Uninstall.exe'
        copied = self.report / (label + '-uninstaller.exe')
        with source.open('rb') as original, copied.open('xb') as out:
            shutil.copyfileobj(original, out)
        require(digest(source) == digest(copied), 'Uninstaller copy differs')
        command = subprocess.list2cmdline([str(copied), '/S']) + ' _?=' + str(self.target)
        self.command(command, label, installer=True)

    # Run the installed CLI smoke check and record its result
    def smoke(self, label, fixture, expected_version):
        binary = self.target / 'stream-dock-manager-cli.exe'
        version = self.command([binary, '--version'], label + '-version')
        require(version.strip() == 'stream-dock-manager-cli ' + expected_version, 'Installed CLI version differs')
        help_text = self.command([binary, '--help'], label + '-help')
        require('Usage:' in help_text and 'models' in help_text and '--config' in help_text,
                'Installed CLI help is incomplete')
        models = json.loads(self.command([binary, '--config', fixture, 'models'], label + '-models'))
        require(isinstance(models.get('implemented'), list) and len(models['implemented']) > 0
                and isinstance(models.get('catalogue'), dict)
                and isinstance(models['catalogue'].get('models'), list)
                and len(models['catalogue']['models']) > 0,
                'Installed CLI returned no hardware descriptors')


# Cleanup authority comes only from a verified state reached by this run
def lifecycle(native, older, current, report, fixture, recheck):
    """Cleanup authority comes only from a verified state reached by this run."""
    original = native.preserved()
    baseline = original
    fixture_before = inventory(fixture)
    report['preserved_before'] = preservation_receipt(original)
    report['phases'] = []
    owned = None
    marker = None
    try:
        report['initial_state'] = native.state()
        validate_absent(report['initial_state'])
        marker = native.config_marker()
        baseline = native.preserved()
        report['config_retention'] = {
            'method': 'owned marker in previously absent real config' if marker else 'existing real configuration read-only',
            'existing_file_count': sum(sum('sha256' in item for item in (original.get(name) or {}).values())
                                       for name in ('manager_roaming', 'manager_local')),
            'marker': None if marker is None else {'name': marker['path'].name, 'sha256': marker['sha256'],
                                                  'created_parent_count': len(marker['created'])},
            'positive_file_retention_exercised': marker is not None or any(
                'sha256' in item for name in ('manager_roaming', 'manager_local')
                for item in (original.get(name) or {}).values())}
        for label, bundle in [('older-install', older), ('same-version-replacement', current),
                              ('reinstall', current)]:
            recheck()
            before = native.state()
            if label == 'same-version-replacement':
                validate_installed(before, older, native.target)
            else:
                validate_absent(before)
            # Once a mutation starts, any previous cleanup proof is stale
            owned = None
            native.install(bundle, 'exe-' + label)
            state = native.state()
            validate_installed(state, bundle, native.target)
            owned = state
            native.smoke('exe-' + label, fixture, bundle['inputs']['catalog']['version'])
            require(native.preserved() == baseline and inventory(fixture) == fixture_before,
                    'Manager configuration, PATH or owned fixture changed')
            report['phases'].append({'format': 'exe', 'phase': label, 'source': bundle['inputs']['catalog']['commit'],
                                     'installed': state, 'metadata_commands_passed': True})
            if label != 'older-install':
                require(native.state() == state, 'Installation changed before owned uninstall')
                native.uninstall('exe-' + label + '-uninstall')
                owned = None
                validate_absent(native.state())
                report['phases'].append({'format': 'exe', 'phase': label + '-uninstall', 'absent': True})
        report['status'] = 'passed'
    except BaseException as error:
        report['status'], report['error'] = 'failed', f'{type(error).__name__}: {error}'
        if owned is not None and not native.pending:
            try:
                recheck()
                require(native.state() == owned, 'Cleanup ownership proof changed; retained for inspection')
                native.uninstall('exe-failure-cleanup')
                validate_absent(native.state())
                report['failure_cleanup'] = 'verified owned installation removed'
            except BaseException as cleanup_error:
                report['cleanup_error'] = f'{type(cleanup_error).__name__}: {cleanup_error}'
        else:
            report['failure_cleanup'] = 'not attempted: no current proof of completed owned installation'
    finally:
        try:
            report['final_state'] = native.state()
            final_preserved = native.preserved()
            require(final_preserved == baseline and inventory(fixture) == fixture_before,
                    'Manager configuration, PATH or fixture changed')
            require(not native.pending, 'Installer may still be running; real config marker retained')
            remove_config_marker(marker)
            restored = native.preserved()
            report['preserved_after'] = preservation_receipt(restored)
            require(restored == original, 'Original Manager configuration/PATH state was not restored')
            require(report['status'] != 'passed' or report['final_state'] ==
                    {'tree': None, 'related': [], 'registry': {}, 'processes': []},
                    'Final cleanup is incomplete')
        except BaseException as error:
            report['status'], report['final_check_error'] = 'failed', f'{type(error).__name__}: {error}'
    return report['status'] == 'passed'


# Inspect the installer and current state without performing lifecycle operations
def inspect_only(native, report, recheck):
    report['scope'] = 'Read-only Windows CLI installer state inspection'
    report['installer_mutations'] = False
    recheck()
    report['state_observation'] = native.state()
    try:
        report['preservation_observation'] = preservation_receipt(native.preserved())
    except (OSError, ValueError) as error:
        report['preservation_observation_error'] = {'type': type(error).__name__,
                                                    'errno': getattr(error, 'errno', None)}
    recheck()
    report['status'] = 'state-inspected'


# Parse options and run the qualify windows cli workflow
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('inputs', 'older-inputs', 'report-dir'):
        parser.add_argument('--' + name, type=Path, required=True)
    for name in ('commit', 'build', 'older-commit', 'older-build'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--repository', required=True)
    parser.add_argument('--inspect-only', action='store_true', help='Read installer state only; never install, run CLI or clean up')
    args = parser.parse_args()
    # Stop the owned qualification operation when interrupted
    def interrupted(_signal, _frame):
        raise KeyboardInterrupt('Qualification interrupted by signal')
    signal.signal(signal.SIGTERM, interrupted)
    report_dir = no_links(args.report_dir)
    output_root = no_links(Path(__file__).absolute().parent.parent / 'artifacts')
    require(report_dir.is_relative_to(output_root) and report_dir != output_root,
            'Report directory must be a new directory inside this checkout artifacts/')
    report_dir.mkdir(parents=True, exist_ok=False)
    report = {'schema_version': 1, 'status': 'failed', 'scope': 'Windows native CLI EXE lifecycle; metadata commands only',
              'host': {'platform': platform.platform(), 'machine': platform.machine(), 'python': sys.version},
              'qualifier_sha256': digest(Path(__file__)), 'input_verifier_sha256': digest(Path(installer_inputs.__file__)),
              'started_at': datetime.now(timezone.utc).isoformat(),
              'selected': {'repository': args.repository, 'current_commit': args.commit, 'current_build': args.build,
                           'older_commit': args.older_commit, 'older_build': args.older_build}}
    native = None
    try:
        # Load and validate package metadata configuration
        def load(path, commit, build):
            return installer_inputs.load_inputs(path, repository=args.repository, commit=commit,
                                               build=build, osname='windows', product='manager-cli')
        older_inputs = load(args.older_inputs, args.older_commit, args.older_build)
        inputs = load(args.inputs, args.commit, args.build)
        require(args.commit != args.older_commit and args.build != args.older_build,
                'Replacement requires two distinct source builds')
        require(older_inputs['catalog']['version'] == inputs['catalog']['version'],
                'Qualification requires same-version replacement inputs')
        source_texts = {'older': {}, 'current': {}}
        older = {'inputs': older_inputs, 'payload': zip_payload(older_inputs, document_report=source_texts['older'])}
        current = {'inputs': inputs, 'payload': zip_payload(inputs, document_report=source_texts['current'])}
        require(older['payload'] != current['payload'], 'Replacement payloads must differ')
        report['inputs'] = {'older': older_inputs['receipt'], 'current': inputs['receipt']}
        report['source_texts'] = source_texts
        native = NativeWindows(report_dir)
        # Verify protected state again before accepting the qualification result
        def recheck():
            require(load(args.older_inputs, args.older_commit, args.older_build) == older_inputs
                    and load(args.inputs, args.commit, args.build) == inputs, 'Inputs changed during qualification')
        if args.inspect_only:
            inspect_only(native, report, recheck)
        else:
            fixture = report_dir / 'owned-config-fixture'
            fixture.mkdir()
            (fixture / 'sentinel').write_text('Metadata commands must not open a Runtime.\n')
            lifecycle(native, older, current, report, fixture, recheck)
    except BaseException as error:
        report['status'], report['error'] = 'failed', f'{type(error).__name__}: {error}'
    finally:
        if native is not None:
            if native.pending:
                report['installer_timeout'] = 'An installer may still be running. Do not rerun or remove remnants until it exits and ownership is reviewed.'
                # Preserve an immutable prefix snapshot. Original logs may still
                # grow; exclude those paths from the final checksum manifest
                report['mutable_logs_excluded_from_checksums'] = [p.name for p in native.pending_logs]
                for path in native.pending_logs:
                    try:
                        shutil.copyfile(path, path.with_name(path.name + '.timeout-snapshot'))
                    except OSError as error:
                        report.setdefault('timeout_snapshot_errors', []).append({'file': path.name, 'error': str(error)})
            try:
                native.close()
            except BaseException as error:
                report['status'], report['close_error'] = 'failed', str(error)
        report['completed_at'] = datetime.now(timezone.utc).isoformat()
        (report_dir / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
        excluded = report.get('mutable_logs_excluded_from_checksums', [])
        entries = sorted(p for p in report_dir.rglob('*') if p.is_file() and p.name not in excluded)
        (report_dir / 'SHA256SUMS').write_text(''.join(f'{digest(p)}  {p.relative_to(report_dir).as_posix()}\n' for p in entries))
    print(json.dumps({'status': report['status'], 'report': str(report_dir / 'report.json')}))
    return 0 if report['status'] == ('state-inspected' if args.inspect_only else 'passed') else 1


if __name__ == '__main__':
    raise SystemExit(main())
