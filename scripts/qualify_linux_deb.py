#!/usr/bin/env python3
"""Exercise verified DEBs only inside a disposable Ubuntu 24.04 container.

This does not launch the desktop, open devices, or qualify a graphical session.
Every command and failed phase is retained; dispose of the container afterwards.
"""
import argparse
import configparser
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import selectors
import shutil
import signal
import stat
import subprocess
import tarfile
import time
import uuid

from packaging_files import sha256_file

from native_build import LINUX_BASELINE_IMAGE, validate_glibc_requirements, validate_linux_userspace
from package_cli_native import RELOAD_RULES
import package_metadata

PRODUCTS = {'manager': 'stream-dock-manager', 'manager-cli': 'stream-dock-manager-cli'}
BINARIES = {'manager': '/usr/bin/stream-dock-manager', 'manager-cli': '/usr/bin/stream-dock-manager-cli'}
DOCUMENTS = {
    'manager': {'LICENSE': '/usr/lib/Stream Dock Manager/notices/LICENSE',
                'THIRD_PARTY_NOTICES.md': '/usr/lib/Stream Dock Manager/notices/THIRD_PARTY_NOTICES.md',
                'INSTALL-INSTRUCTIONS.md': '/usr/lib/Stream Dock Manager/INSTALL-INSTRUCTIONS.md',
                '60-stream-dock-manager.rules': '/usr/lib/udev/rules.d/60-stream-dock-manager.rules'},
    'manager-cli': {'LICENSE': '/usr/share/doc/stream-dock-manager-cli/LICENSE',
                    'THIRD_PARTY_NOTICES.md': '/usr/share/doc/stream-dock-manager-cli/THIRD_PARTY_NOTICES.md',
                    'INSTALL-INSTRUCTIONS.md': '/usr/share/doc/stream-dock-manager-cli/INSTALL-INSTRUCTIONS.md',
                    '60-stream-dock-manager.rules': '/usr/lib/udev/rules.d/60-stream-dock-manager-cli.rules'},
}
DESKTOP_ENTRY = '/usr/share/applications/Stream Dock Manager.desktop'
PORTAL_ENTRY = '/usr/share/applications/app.streamdockmanager.manager.desktop'
METAINFO = '/usr/share/metainfo/app.streamdockmanager.manager.metainfo.xml'
METADATA = '/usr/share/doc/stream-dock-manager/package-metadata.json'
METADATA_RESOURCE = '/usr/lib/Stream Dock Manager/notices/package-metadata.json'
BASE_ENV = {'PATH': '/usr/sbin:/usr/bin:/sbin:/bin', 'LC_ALL': 'C', 'LANG': 'C',
            'DEBIAN_FRONTEND': 'noninteractive', 'APT_LISTCHANGES_FRONTEND': 'none'}
MAX_PAYLOAD = 256 * 1024 * 1024


# Calculate the file content digest used for integrity checks
def sha256(path):
    return sha256_file(path)


# Serialize the evidence document to its selected output file
def write_json(path, value):
    temporary = path.with_suffix(path.suffix + '.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n', encoding='utf-8')
    temporary.replace(path)


# Require the disposable environment intended for Linux package qualification
def validate_environment(evidence):
    validate_linux_userspace(**evidence['userspace'])
    if evidence['system'] != 'Linux' or evidence['machine'] not in ('x86_64', 'amd64') or evidence['uid'] != 0:
        raise ValueError('Qualification requires native Linux x86-64 and container root')
    if evidence['root_filesystem'] not in ('overlay', 'fuse.overlayfs'):
        raise ValueError('Qualification requires a disposable overlay container root, never a host root')
    # Reject privileged containers and device/daemon passthrough independently
    # of the declarative image identity supplied by the workflow
    forbidden_caps = sum(1 << bit for bit in (16, 17, 19, 21, 22))
    if evidence['capabilities'] & forbidden_caps:
        raise ValueError('Privileged container capabilities are forbidden')
    if evidence['unsafe_paths'] or evidence['unsafe_mounts'] or evidence['session_variables']:
        raise ValueError('Device/daemon mounts and graphical/runtime sessions are forbidden')


# Detect mounts that would expose protected host paths to qualification
def unsafe_mounts(mounts):
    # The root overlay alone is insufficient: bind-mounting /usr or /etc would
    # redirect an otherwise container-local APT transaction onto the host
    protected = [PurePosixPath(p) for p in ('/usr', '/etc', '/var', '/bin', '/sbin', '/lib', '/lib64', '/run', '/root', '/home')]
    permitted_files = {'/etc/hosts', '/etc/hostname', '/etc/resolv.conf'}
    unsafe = []
    for point, filesystem in mounts:
        path = PurePosixPath(point)
        if point == '/' or point in permitted_files:
            continue
        if (any(path == p or path.is_relative_to(p) or p.is_relative_to(path) for p in protected)
                or point.startswith(('/dev/bus/', '/dev/hidraw'))
                or (point == '/dev' and filesystem != 'tmpfs')):
            unsafe.append(point)
    return unsafe


# Record the actual operating environment used for package qualification
def environment_evidence():
    if platform.system() != 'Linux':
        raise ValueError('This qualifier may only run inside its disposable Linux container')
    release = platform.freedesktop_os_release()
    libc, version = platform.libc_ver()
    mounts = []
    for line in Path('/proc/self/mountinfo').read_text().splitlines():
        before, after = line.split(' - ', 1)
        point = re.sub(r'\\([0-7]{3})', lambda m: chr(int(m[1], 8)), before.split()[4])
        mounts.append((point, after.split()[0]))
    roots = [kind for point, kind in mounts if point == '/']
    caps = re.search(r'^CapEff:\s*([0-9a-fA-F]+)$', Path('/proc/self/status').read_text(), re.M)
    return {
        'system': platform.system(), 'machine': platform.machine(), 'uid': os.geteuid(),
        'kernel': platform.release(), 'root_filesystem': roots[0] if len(roots) == 1 else '',
        'capabilities': int(caps[1], 16) if caps else (1 << 64) - 1,
        'unsafe_paths': [str(p) for pattern in ('/dev/hidraw*', '/dev/bus/usb', '/dev/uinput', '/dev/input', '/dev/snd', '/dev/dri',
                                              '/var/run/docker.sock', '/run/containerd/containerd.sock',
                                              '/run/udev/control', '/run/dbus/system_bus_socket')
                         for p in Path('/').glob(pattern.lstrip('/'))],
        'unsafe_mounts': unsafe_mounts(mounts),
        'session_variables': [name for name in ('DISPLAY', 'WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS', 'XDG_RUNTIME_DIR')
                              if os.environ.get(name)],
        'userspace': {'distribution': release.get('ID', ''), 'version': release.get('VERSION_ID', ''),
                      'libc': libc, 'libc_version': version,
                      'image': os.environ.get('STREAM_DOCK_MANAGER_LINUX_BASELINE_IMAGE', ''),
                      'container': Path('/.dockerenv').is_file()},
    }


class Commands:
    """Bounded, retained stdout/stderr; kill only this command's process group."""
    # Create the owned command-log directory and retain the qualification environment
    def __init__(self, directory, *, environment=None):
        self.directory = directory
        self.environment = BASE_ENV if environment is None else environment
        directory.mkdir()
        self.number = 0

    # Run the selected command and propagate failure to the calling workflow
    def run(self, label, args, *, timeout=900, limit=8 * 1024 * 1024, allowed=(0,)):
        self.number += 1
        if not re.fullmatch(r'[a-z0-9-]+', label):
            raise ValueError('Invalid command log label')
        stem = self.directory / f'{self.number:03d}-{label}'
        record = {'command': [str(a) for a in args], 'timeout_seconds': timeout,
                  'stdout': stem.name + '.stdout', 'stderr': stem.name + '.stderr', 'passed': False}
        start = time.monotonic()
        process = None
        try:
            with stem.with_suffix('.stdout').open('xb') as stdout, stem.with_suffix('.stderr').open('xb') as stderr:
                process = subprocess.Popen(record['command'], stdin=subprocess.DEVNULL,
                                           stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                           env=self.environment, start_new_session=True)
                with selectors.DefaultSelector() as selector:
                    selector.register(process.stdout, selectors.EVENT_READ, stdout)
                    selector.register(process.stderr, selectors.EVENT_READ, stderr)
                    total = 0
                    while selector.get_map():
                        if time.monotonic() - start > timeout:
                            raise TimeoutError(f'{label} exceeded its {timeout}-second deadline')
                        for key, _ in selector.select(0.2):
                            data = os.read(key.fd, 65536)
                            if not data:
                                selector.unregister(key.fileobj)
                                continue
                            remaining = limit - total
                            key.data.write(data[:remaining])
                            total += len(data)
                            if total > limit:
                                raise ValueError(f'{label} exceeded its retained-output limit')
                    record['returncode'] = process.wait(timeout=max(0.1, timeout - (time.monotonic() - start)))
                if record['returncode'] not in allowed:
                    raise ValueError(f'{label} failed with exit {record["returncode"]}; see retained command output')
                record['passed'] = True
        except BaseException as error:
            record['error'] = str(error)
            raise
        finally:
            if process is not None:
                # A descendant could retain captured pipes after its parent exits
                # This is the newly created process group, never a name/PID lookup
                if not record['passed']:
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    process.wait(timeout=2)
                process.stdout.close()
                process.stderr.close()
            record['elapsed_seconds'] = round(time.monotonic() - start, 3)
            write_json(stem.with_suffix('.json'), record)
        return {'stdout': stem.with_suffix('.stdout'), 'stderr': stem.with_suffix('.stderr'), **record}

    # Read the command output as text for qualification evidence
    def text(self, label, args, **kwargs):
        result = self.run(label, args, **kwargs)
        return (self.directory / result['stdout']).read_text(encoding='utf-8')


# Check apt-get's C-locale simulation before any actual transaction
def validate_transaction(output, operation, intended, installed):
    """Check apt-get's C-locale simulation before any actual transaction."""
    actions = []
    for line in output.splitlines():
        match = re.match(r'^(Inst|Conf|Remv|Purg) (\S+)(?: \[([^]]+)\])?(?: \((\S+)[^)]*\))?', line)
        if not match:
            continue
        kind, package, old, new = match.groups()
        package = package.removesuffix(':amd64')
        if not re.fullmatch(r'[a-z0-9][a-z0-9+.-]*', package):
            raise ValueError('APT proposed an unexpected package architecture or name')
        if kind == 'Purg' or (kind == 'Remv' and (operation != 'remove' or package not in intended)):
            raise ValueError('APT proposed an unrelated removal or purge')
        if kind == 'Inst':
            if operation == 'remove' or not new:
                raise ValueError('APT proposed an unexpected installation')
            if package in installed and package not in intended:
                raise ValueError('APT proposed reinstalling/upgrading an unrelated installed package')
            if old and package not in intended:
                raise ValueError('APT proposed an unrelated upgrade')
        if kind == 'Conf' and package in installed and package not in intended:
            raise ValueError('APT proposed reconfiguring an unrelated installed package')
        actions.append({'operation': kind, 'package': package, 'old': old, 'new': new})
    requested = {a['package'] for a in actions if a['operation'] == ('Remv' if operation == 'remove' else 'Inst')}
    if not intended.issubset(requested):
        raise ValueError('APT simulation did not include every intended product')
    return actions


# Apply the validated transaction inside the disposable qualification environment
def apply_transaction(commands, label, selection, before):
    intended = set(PRODUCTS.values())
    operation = 'install' if selection else 'remove'
    if selection:
        for item in selection.values():
            if sha256(item['package']) != item['sha256']:
                raise ValueError('Staged DEB changed before installation')
        tail = ['--reinstall', 'install', *[str(item['package']) for item in selection.values()]]
    else:
        tail = ['remove', *sorted(intended)]
    common = ['apt-get', '-o', 'Dpkg::Use-Pty=0', '--no-install-recommends']
    preview = commands.text(f'{label}-preview', [*common, '--simulate', *tail], timeout=120)
    actions = validate_transaction(preview, operation, intended, before)
    for action in actions:
        old_version = before.get(action['package'], action['old'])
        if action['operation'] == 'Inst' and old_version:
            commands.run(f'{label}-non-downgrade', ['dpkg', '--compare-versions', action['new'], 'ge', old_version], timeout=10)
    flags = ['--yes', '--no-remove'] if selection else ['--yes']
    commands.run(label, [*common, *flags, *tail])
    return actions


# Compare actual package changes with the approved transaction plan
def verify_transaction_changes(before, after, actions):
    intended = set(PRODUCTS.values())
    if any(after.get(package) != version for package, version in before.items() if package not in intended):
        raise ValueError('APT changed an unrelated installed package')
    allowed_new = {action['package'] for action in actions if action['operation'] == 'Inst'}
    if set(after) - set(before) - allowed_new:
        raise ValueError('APT installed an unpreviewed package')


# Inspect Debian package control metadata without installing it
def inspect_control(tar_path, product):
    entries = {}
    with tarfile.open(tar_path, 'r:') as archive:
        for member in archive:
            if member.name in ('.', './') and member.isdir():
                continue
            name = member.name.removeprefix('./')
            if '/' in name or name in entries or not member.isfile() or member.size > 1024 * 1024:
                raise ValueError('Unexpected DEB control archive entry')
            entries[name] = archive.extractfile(member).read()
    expected = {'control', 'md5sums'} if product == 'manager' else {'control', 'postinst', 'postrm'}
    if set(entries) != expected:
        raise ValueError('Unexpected maintainer scripts or control fields in DEB')
    if product == 'manager-cli':
        allowed_script = ('#!/bin/sh\nset -e\n' + RELOAD_RULES).encode()
        if any(entries[name] != allowed_script for name in ('postinst', 'postrm')):
            raise ValueError('CLI maintainer scripts must only reload udev rules')
    return {name: hashlib.sha256(data).hexdigest() for name, data in entries.items()}


# Resolve a package payload member inside its extraction root
def payload_path(name):
    if name.startswith('./'):
        name = name[2:]
    path = PurePosixPath(name)
    if not name or path.is_absolute() or '..' in path.parts or str(path) != name or '\\' in name or any(ord(c) < 32 for c in name):
        raise ValueError('Unsafe DEB payload path')
    return '/' + name


# Inspect the extracted Debian payload and its ownership metadata
def inspect_payload(tar_path, product, source_files):
    files, directories, total, snippets = {}, set(), 0, {}
    with tarfile.open(tar_path, 'r:') as archive:
        for number, member in enumerate(archive):
            if number >= 1024:
                raise ValueError('DEB payload has too many entries')
            if member.name in ('.', './') and member.isdir():
                continue
            path = payload_path(member.name)
            if path in files or path in directories or member.uid != 0 or member.gid != 0:
                raise ValueError('DEB payload has duplicate paths or non-root ownership')
            if member.isdir():
                if member.mode != 0o755:
                    raise ValueError('DEB directories must be mode 0755')
                directories.add(path)
                continue
            if not member.isfile() or member.mode not in (0o644, 0o755):
                raise ValueError('DEB payload must contain only ordinary files/directories with safe modes')
            total += member.size
            if member.size < 0 or total > MAX_PAYLOAD:
                raise ValueError('DEB payload exceeds the bounded inventory size')
            digest, head = hashlib.sha256(), bytearray()
            with archive.extractfile(member) as source:
                while chunk := source.read(65536):
                    digest.update(chunk)
                    if len(head) < 65536:
                        head.extend(chunk[:65536 - len(head)])
            files[path] = {'sha256': digest.hexdigest(), 'size': member.size, 'mode': member.mode}
            snippets[path] = bytes(head)
    required = {BINARIES[product], *DOCUMENTS[product].values()}
    if product == 'manager':
        required.update({DESKTOP_ENTRY, PORTAL_ENTRY, METAINFO, METADATA, METADATA_RESOURCE})
        allowed = lambda p: p in required or re.fullmatch(r'/usr/share/icons/hicolor/\d+x\d+(?:@\d+)?/apps/stream-dock-manager\.png', p)
    else:
        required.add('/usr/share/doc/stream-dock-manager-cli/README.md')
        allowed = lambda p: p in required
    if not required.issubset(files) or any(not allowed(path) for path in files):
        raise ValueError('DEB payload is missing required files or owns unexpected paths')
    if product == 'manager':
        if any(files[path]['size'] > 65536 for path in (METAINFO, METADATA, METADATA_RESOURCE)):
            raise ValueError('Package metadata exceeds validation limit')
        if snippets[METADATA] != snippets[METADATA_RESOURCE]:
            raise ValueError('Package metadata resources differ')
        document = json.loads(snippets[METADATA])
        if set(document) != {'version', 'product', *package_metadata.FIELDS} or document['product'] != 'desktop':
            raise ValueError('Invalid package metadata identity')
        if not re.fullmatch(r'(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)', document['version']):
            raise ValueError('Invalid package metadata version')
        if 'VERSION' in source_files and document['version'] != source_files['VERSION'].read_text().strip():
            raise ValueError('Package metadata version differs from source')
        data = package_metadata.validate({key: document[key] for key in package_metadata.FIELDS})
        if snippets[METAINFO] != package_metadata.appstream(data, document['version']).encode('utf-8'):
            raise ValueError('AppStream differs from package metadata')
    ancestors = {str(parent) for path in files for parent in PurePosixPath(path).parents}
    if not directories.issubset(ancestors):
        raise ValueError('DEB payload contains unrelated directories')
    for source, destination in DOCUMENTS[product].items():
        if files[destination]['sha256'] != sha256(source_files[source]):
            raise ValueError(f'DEB source resource differs: {destination}')
    for path, info in files.items():
        if info['mode'] != (0o755 if path == BINARIES[product] else 0o644):
            raise ValueError('Unexpected DEB executable/resource mode')
    binary = snippets[BINARIES[product]]
    if binary[:6] != b'\x7fELF\x02\x01' or len(binary) < 20 or binary[18:20] != b'\x3e\x00':
        raise ValueError('DEB must contain an ELF64 x86-64 executable')
    if product == 'manager':
        entry = configparser.ConfigParser(interpolation=None, strict=True)
        entry.read_string(snippets[DESKTOP_ENTRY].decode('utf-8'))
        launcher = entry['Desktop Entry']
        if (launcher.get('Type'), launcher.get('Exec'), launcher.get('MimeType')) != (
            'Application', 'stream-dock-manager %u', 'x-scheme-handler/stream-dock-manager;',
        ):
            raise ValueError('Desktop entry must forward the Manager catalog URL')
        portal = configparser.ConfigParser(interpolation=None, strict=True)
        portal.read_string(snippets[PORTAL_ENTRY].decode('utf-8'))
        identity = portal['Desktop Entry']
        if (identity.get('Type'), identity.get('Exec'), identity.get('NoDisplay')) != (
            'Application', 'stream-dock-manager', 'true',
        ):
            raise ValueError('Missing hidden Manager portal identity')
    return {'files': files, 'directories': sorted(directories), 'total_file_bytes': total}


# Compare installed files with the expected package payload
def verify_files(manifest, *, root=Path('/'), absent=False):
    for relative, expected in manifest['files'].items():
        path = root / relative.lstrip('/')
        if absent:
            if os.path.lexists(path):
                raise ValueError(f'Removed package payload remains: {relative}')
        else:
            for part in (path, *path.parents):
                if part == root:
                    break
                if part.is_symlink():
                    raise ValueError(f'Installed package path is a symlink: {relative}')
            if not path.is_file() or path.stat().st_size != expected['size'] or stat.S_IMODE(path.stat().st_mode) != expected['mode'] or sha256(path) != expected['sha256']:
                raise ValueError(f'Installed package bytes/mode differ: {relative}')


# Record file identity and digests for later preservation checks
def tree_identity(root):
    result = {}
    total = 0
    for path in sorted(root.rglob('*')):
        if path.is_symlink() or not (path.is_file() or path.is_dir()):
            raise ValueError('Unexpected isolated configuration entry')
        info = path.stat()
        total += info.st_size if path.is_file() else 0
        if len(result) >= 4096 or total > 64 * 1024 * 1024:
            raise ValueError('Isolated configuration exceeds bounded fixture limits')
        result[path.relative_to(root).as_posix()] = {'mode': stat.S_IMODE(info.st_mode),
                                                   'uid': info.st_uid, 'gid': info.st_gid,
                                                   'sha256': sha256(path) if path.is_file() else None}
    return result


# Read the package-manager status for the selected package
def package_status(commands, label, *, require_products_absent=False):
    output = commands.text(label, ['dpkg-query', '-W', '-f=${binary:Package}\t${Version}\t${db:Status-Abbrev}\n'], timeout=60)
    result = {}
    for line in output.splitlines():
        package, version, state = line.split('\t')
        if require_products_absent and package.removesuffix(':amd64') in PRODUCTS.values():
            raise ValueError('Product has pre-existing package database state; use a new container')
        if state.startswith('ii'):
            result[package.removesuffix(':amd64')] = version
    return result


# Exercise installation and removal inside the explicitly selected test environment
def lifecycle(args, report, commands, save):
    from installer_inputs import load_inputs
    report['environment'] = environment_evidence()
    validate_environment(report['environment'])
    report['container_disposal_required'] = True
    if commands.text('audit-dpkg', ['dpkg', '--audit'], timeout=60).strip():
        raise ValueError('Container package database is not initially healthy')
    initial = package_status(commands, 'initial-packages', require_products_absent=True)
    if set(PRODUCTS.values()) & initial.keys():
        raise ValueError('Product package already installed; use a new disposable container')
    report['phase'] = 'validate-inputs'
    save()
    selections = [('candidate', args.commit, args.build, args.desktop_inputs, args.cli_inputs)]
    older = [args.older_commit, args.older_build, args.older_desktop_inputs, args.older_cli_inputs]
    if any(older) and not all(older):
        raise ValueError('Older qualification requires both product inputs and exact older commit/build')
    if all(older):
        if args.older_commit == args.commit:
            raise ValueError('Older candidate must have a distinct verified source commit')
        selections.insert(0, ('older', *older))
    candidates = {}
    report['inputs'] = {}
    for stage, commit, build, desktop, cli in selections:
        candidates[stage] = {}
        for product, directory in [('manager', desktop), ('manager-cli', cli)]:
            evidence = load_inputs(directory, repository=args.repository, commit=commit, build=build,
                                   osname='linux', product=product)
            package = evidence['assets'].get('.deb')
            if package is None:
                raise ValueError('Validated input has no DEB')
            work = args.report_dir / f'{stage}-{product}'
            work.mkdir()
            copy = work / package.name
            shutil.copyfile(package, copy)
            selected = [asset for asset in evidence['catalog']['assets'] if asset['name'] == package.name]
            if (len(selected) != 1 or sha256(copy) != selected[0]['sha256']
                    or copy.stat().st_size != selected[0]['size'] or sha256(copy) != sha256(package)):
                raise ValueError('Input package changed while staging')
            copy.chmod(0o444)
            control = commands.text(f'{stage}-{product}-control', ['dpkg-deb', '--field', str(copy)], timeout=60)
            fields = dict(re.findall(r'^([A-Za-z-]+): (.*)$', control, re.M))
            if (fields.get('Package'), fields.get('Version'), fields.get('Architecture')) != (PRODUCTS[product], evidence['catalog']['version'], 'amd64'):
                raise ValueError('DEB package/version/architecture differs from its catalog')
            control_tar = commands.run(f'{stage}-{product}-control-tar', ['dpkg-deb', '--ctrl-tarfile', str(copy)], timeout=60, limit=2 * 1024 * 1024)
            control_hashes = inspect_control(commands.directory / control_tar['stdout'], product)
            tar_result = commands.run(f'{stage}-{product}-payload', ['dpkg-deb', '--fsys-tarfile', str(copy)], timeout=90, limit=MAX_PAYLOAD + 4 * 1024 * 1024)
            tar_path = commands.directory / tar_result['stdout']
            manifest = inspect_payload(tar_path, product, evidence['source_files'])
            # Source notices are revalidated after inspection as well as before it
            load_inputs(directory, repository=args.repository, commit=commit, build=build,
                        osname='linux', product=product)
            binary = work / 'verified-elf'
            with tarfile.open(tar_path, 'r:') as archive:
                member = next(m for m in archive if m.isfile() and payload_path(m.name) == BINARIES[product])
                with archive.extractfile(member) as source, binary.open('xb') as output:
                    shutil.copyfileobj(source, output)
            requirements = commands.text(f'{stage}-{product}-glibc', ['readelf', '--version-info', '--wide', str(binary)], timeout=60)
            validate_glibc_requirements(requirements)
            candidates[stage][product] = {'package': copy, 'sha256': sha256(copy), 'version': fields['Version'], 'manifest': manifest}
            report['inputs'][f'{stage}-{product}'] = {'receipt': evidence['receipt'], 'package_sha256': sha256(copy),
                                                    'control_sha256': control_hashes, 'manifest': manifest}
            verify_files(manifest, absent=True)
    for directory in ('/usr/lib/Stream Dock Manager', '/usr/share/doc/stream-dock-manager-cli'):
        if os.path.lexists(directory):
            raise ValueError(f'Product namespace already exists: {directory}')
    for pair in candidates.values():
        if pair['manager']['manifest']['files'].keys() & pair['manager-cli']['manifest']['files'].keys():
            raise ValueError('Desktop and CLI packages have overlapping ownership')
    if 'older' in candidates:
        for product in PRODUCTS:
            old, new = candidates['older'][product], candidates['candidate'][product]
            commands.run(f'{product}-non-downgrade', ['dpkg', '--compare-versions', new['version'], 'ge', old['version']], timeout=10)
            if old['manifest']['files'] == new['manifest']['files']:
                raise ValueError('Older/candidate payloads are identical; changed-payload upgrade cannot be claimed')
    report['upgrade'] = {'attempted': False, 'passed': False,
                         'status': 'compatible_changed_payload_inputs_verified' if 'older' in candidates
                         else 'not_attempted_no_verified_compatible_older_candidate'}
    report['phase'] = 'prepare-isolated-fixture'
    save()
    # Dependency maintainer scripts must not start container daemons. Do not
    # replace a pre-existing policy whose meaning this qualifier cannot verify
    policy = Path('/usr/sbin/policy-rc.d')
    policy_bytes = b'#!/bin/sh\nexit 101\n'
    if os.path.lexists(policy):
        if policy.is_symlink() or policy.read_bytes() != policy_bytes or stat.S_IMODE(policy.stat().st_mode) != 0o755:
            raise ValueError('Use a fresh container without a custom service-start policy')
    else:
        with policy.open('xb') as output:
            output.write(policy_bytes)
        policy.chmod(0o755)
    username = 'sdqual-' + uuid.uuid4().hex[:12]
    home = args.report_dir / 'isolated-home'
    home.mkdir(mode=0o700)
    commands.run('create-isolated-user', ['useradd', '--system', '--no-create-home', '--home-dir', str(home), '--shell', '/usr/sbin/nologin', username], timeout=30)
    import pwd
    user = pwd.getpwnam(username)
    if user.pw_uid == 0 or user.pw_dir != str(home):
        raise ValueError('Isolated qualification user was not created correctly')
    config = home / '.config' / 'stream-dock-manager'
    config.mkdir(parents=True)
    sentinel = config / 'qualification-sentinel.json'
    sentinel.write_text(json.dumps({'fixture': 'installer-retention-only', 'source': args.commit}) + '\n')
    sentinel.chmod(0o600)
    for path in [home, *home.rglob('*')]:
        os.chown(path, user.pw_uid, user.pw_gid)
    commands.run('check-isolated-user-access', ['runuser', '-u', username, '--', 'test', '-r', str(sentinel)], timeout=10)
    identity = tree_identity(home)
    report['isolated_user'] = {'name': username, 'uid': user.pw_uid, 'home': str(home), 'sentinel_sha256': sha256(sentinel)}

    # Check that protected fixture state remains intact
    def sentinel_check():
        if tree_identity(home) != identity:
            raise ValueError('Isolated HOME/configuration changed during package qualification')

    # Run a package-manager transaction and capture its evidence
    def transaction(label, selection=None):
        report['phase'] = label
        if label == 'replace-candidate':
            report['upgrade']['attempted'] = True
        save()
        before = package_status(commands, f'{label}-before')
        intended = set(PRODUCTS.values())
        actions = apply_transaction(commands, label, selection, before)
        after = package_status(commands, f'{label}-after')
        verify_transaction_changes(before, after, actions)
        if selection:
            for product, item in selection.items():
                if after.get(PRODUCTS[product]) != item['version']:
                    raise ValueError('Installed product version differs from verified candidate')
                verify_files(item['manifest'])
                owned = commands.text(f'{label}-{product}-ownership', ['dpkg-query', '-L', PRODUCTS[product]], timeout=30)
                expected_paths = set(item['manifest']['files']) | set(item['manifest']['directories'])
                if set(owned.splitlines()) - {'/.', '/'} != expected_paths:
                    raise ValueError('Installed package ownership differs from the inspected DEB')
            for argument, output_name in [('--version', 'version'), ('--help', 'help'), ('models', 'models')]:
                output = commands.text(f'{label}-cli-{output_name}', ['runuser', '-u', username, '--', 'env', '-i',
                    f'HOME={home}', f'XDG_CONFIG_HOME={home / ".config"}', 'PATH=/usr/bin:/bin', 'LC_ALL=C',
                    BINARIES['manager-cli'], argument], timeout=30)
                if argument == '--version' and output.strip() != 'stream-dock-manager-cli ' + selection['manager-cli']['version']:
                    raise ValueError('Installed CLI version smoke differs')
                if argument == '--help' and ('Usage:' not in output or 'models' not in output):
                    raise ValueError('Installed CLI help smoke is incomplete')
                if argument == 'models':
                    models = json.loads(output)
                    if not models.get('catalogue', {}).get('models') or not models.get('implemented'):
                        raise ValueError('Installed CLI models smoke is empty')
        else:
            if intended & after.keys():
                raise ValueError('Product package remains installed after removal')
            for pair in candidates.values():
                for item in pair.values():
                    verify_files(item['manifest'], absent=True)
        sentinel_check()
        if policy.is_symlink() or policy.read_bytes() != policy_bytes:
            raise ValueError('Container service-start policy changed')
        report['completed_phases'].append({'name': label, 'apt_actions': actions, 'sentinel_unchanged': True})
        if label == 'replace-candidate':
            report['upgrade'].update(passed=True, status='changed_payload_replacement_passed')
        save()

    transaction('install-baseline', candidates.get('older', candidates['candidate']))
    transaction('replace-candidate' if 'older' in candidates else 'reinstall-same-package', candidates['candidate'])
    transaction('remove-products')
    transaction('reinstall-candidate', candidates['candidate'])
    transaction('final-remove-products')
    report['phase'] = 'complete'
    report['passed'] = True
    report['limits'] = ['Disposable Ubuntu 24.04 container package lifecycle only',
                        'No desktop launch, GUI, HID, device permission, Nobara or host-distribution qualification',
                        'Only nonroot CLI --version/--help/models; no Runtime/configuration commands',
                        'Dependencies and isolated user remain inside the disposable container until it is destroyed']


# Parse options and run the qualify linux deb workflow
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('repository', 'commit', 'build'):
        parser.add_argument('--' + name, required=True)
    for name in ('desktop-inputs', 'cli-inputs', 'report-dir'):
        parser.add_argument('--' + name, type=Path, required=True)
    for name in ('older-commit', 'older-build'):
        parser.add_argument('--' + name)
    for name in ('older-desktop-inputs', 'older-cli-inputs'):
        parser.add_argument('--' + name, type=Path)
    parser.add_argument('--allow-disposable-container', action='store_true')
    args = parser.parse_args()
    if not args.allow_disposable_container:
        parser.error('Explicit --allow-disposable-container is required; never run this on a host installation')
    args.report_dir = args.report_dir.absolute()
    if args.report_dir.exists() or args.report_dir.resolve() != args.report_dir or args.report_dir.parent == Path('/'):
        parser.error('Report directory must be a new owned directory without symlink ancestors')
    args.report_dir.mkdir(mode=0o755)
    report = {'schema_version': 1, 'repository': args.repository, 'commit': args.commit, 'build': args.build,
              'passed': False, 'phase': 'preflight', 'completed_phases': [],
              'workflow_commit': os.environ.get('GITHUB_SHA', ''),
              'qualifier_sources': {name: sha256(Path(__file__).parent / name) for name in
                                    ('qualify_linux_deb.py', 'installer_inputs.py', 'native_build.py', 'package_cli_native.py')}}
    save = lambda: write_json(args.report_dir / 'report.json', report)
    save()
    try:
        lifecycle(args, report, Commands(args.report_dir / 'commands'), save)
    except BaseException as error:
        report['error'] = f'{type(error).__name__}: {error}'
        raise
    finally:
        save()


if __name__ == '__main__':
    main()
