#!/usr/bin/env python3
"""Launch the DEB qualifier in an owned disposable Docker child, never the host.

The Actions job itself must run on the host. Its Docker socket is used only by
this launcher and is never mounted or passed into the qualification container.
"""
import argparse
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import stat
import urllib.parse
import uuid

import installer_inputs
from native_build import LINUX_BASELINE_IMAGE
from qualify_linux_deb import Commands, write_json

OWNER_LABEL = 'app.streamdockmanager.qualification.owner'
ROLE_LABEL = 'app.streamdockmanager.qualification.role'
CAPABILITIES = ['CHOWN', 'DAC_OVERRIDE', 'FOWNER', 'FSETID', 'KILL', 'SETGID', 'SETUID', 'SYS_CHROOT', 'SETFCAP', 'AUDIT_WRITE']
BOOTSTRAP = '''set -euo pipefail
trap 'result=$?; trap - EXIT; chown -hRP --preserve-root -- "$RESULT_OWNER" /results || result=1; exit "$result"' EXIT
policy=/usr/sbin/policy-rc.d
if [ -e "$policy" ] || [ -L "$policy" ]; then
    if [ -L "$policy" ] || [ ! -f "$policy" ] || [ "$(stat -c %a "$policy")" != 755 ] || ! printf '#!/bin/sh\\nexit 101\\n' | cmp -s "$policy" -; then
        echo 'Unexpected existing service-start policy; refusing to replace it.' >&2
        exit 1
    fi
else
    printf '#!/bin/sh\\nexit 101\\n' > "$policy"
    chmod 755 "$policy"
fi
apt-get update
DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends ca-certificates python3 binutils passwd util-linux
python3 /qualifier-scripts/qualify_linux_deb.py "$@" --report-dir /results/report --allow-disposable-container
'''


# Validate the directory before using it as an owned work location
def directory(path):
    path = Path(path).absolute()
    if not path.is_dir() or path.resolve() != path:
        raise ValueError('Docker bind sources must be existing directories without linked ancestors')
    if any(c in str(path) for c in ',\r\n\0'):
        raise ValueError('Unsupported character in Docker bind source')
    return path


# Validate a container mount specification before building its arguments
def mount(source, target, readonly):
    return {'source': str(directory(source)), 'target': target, 'readonly': readonly}


# Construct the owned disposable Linux qualification container specification
def specification(root, scripts, arguments, uid, gid, nonce, *, permissions=False):
    if not re.fullmatch(r'[0-9a-f]{32}', nonce) or min(uid, gid) < 0:
        raise ValueError('Invalid owned container identity')
    role = 'permissions' if permissions else 'lifecycle'
    mounts = [mount(root / 'linux-container-results', '/results', False)]
    if permissions:
        entrypoint = '/usr/bin/chown'
        command = ['-hRP', '--preserve-root', '--', f'{uid}:{gid}', '/results']
    else:
        mounts += [mount(scripts, '/qualifier-scripts', True), mount(root / 'candidate', '/inputs/candidate', True)]
        if arguments.older_build:
            mounts.append(mount(root / 'baseline', '/inputs/baseline', True))
        entrypoint = '/bin/bash'
        command = ['-c', BOOTSTRAP, 'qualification', '--repository', arguments.repository,
                   '--commit', arguments.commit, '--build', arguments.build,
                   '--desktop-inputs', '/inputs/candidate/manager', '--cli-inputs', '/inputs/candidate/manager-cli']
        if arguments.older_build:
            command += ['--older-commit', arguments.older_commit, '--older-build', arguments.older_build,
                        '--older-desktop-inputs', '/inputs/baseline/manager', '--older-cli-inputs', '/inputs/baseline/manager-cli']
    return {'name': f'stream-dock-manager-qualification-{nonce}-{role}', 'nonce': nonce, 'role': role,
            'mounts': mounts, 'entrypoint': entrypoint, 'command': command,
            'uid': uid, 'gid': gid, 'image': LINUX_BASELINE_IMAGE,
            'network': 'none' if permissions else 'bridge', 'readonly_root': permissions,
            'capabilities': ['CHOWN', 'DAC_OVERRIDE', 'FOWNER'] if permissions else CAPABILITIES,
            'environment': {'STREAM_DOCK_MANAGER_LINUX_BASELINE_IMAGE': LINUX_BASELINE_IMAGE,
                            'RESULT_OWNER': f'{uid}:{gid}', 'PYTHONDONTWRITEBYTECODE': '1'}}


# Build container-creation arguments from the validated specification
def create_arguments(spec):
    args = ['create', '--name', spec['name'], '--platform', 'linux/amd64', '--user', '0:0',
            '--label', f'{OWNER_LABEL}={spec["nonce"]}', '--label', f'{ROLE_LABEL}={spec["role"]}',
            '--network', spec['network'], '--ipc', 'private', '--security-opt', 'no-new-privileges',
            '--cap-drop', 'ALL', '--pids-limit', '256', '--memory', '3g', '--cpus', '2',
            '--entrypoint', spec['entrypoint']]
    if spec['readonly_root']:
        args += ['--read-only']
    for capability in spec['capabilities']:
        args += ['--cap-add', capability]
    for item in spec['mounts']:
        value = f'type=bind,source={item["source"]},target={item["target"]}'
        args += ['--mount', value + (',readonly' if item['readonly'] else '')]
    for key, value in spec['environment'].items():
        args += ['--env', f'{key}={value}']
    return [*args, spec['image'], *spec['command']]


# Require proof that the qualification container belongs to this invocation
def require_owned(info, spec, expected_id=None):
    labels = info.get('Config', {}).get('Labels', {})
    cid = info.get('Id', '')
    if (not re.fullmatch(r'[0-9a-f]{64}', cid) or (expected_id and cid != expected_id)
            or info.get('Name') != '/' + spec['name']
            or labels.get(OWNER_LABEL) != spec['nonce'] or labels.get(ROLE_LABEL) != spec['role']):
        raise ValueError('Container ownership proof differs; no cleanup is authorized')
    return cid


# Verify the created container matches its requested specification
def validate_created(info, spec, image_id):
    cid = require_owned(info, spec)
    config, host = info['Config'], info['HostConfig']
    if (info.get('Image') != image_id or config.get('Image') != spec['image']
            or config.get('Entrypoint') != [spec['entrypoint']] or config.get('Cmd') != spec['command']
            or config.get('User') != '0:0' or info.get('State', {}).get('Running')):
        raise ValueError('Created child differs from the pinned, not-yet-started specification')
    actual = {(item.get('Source'), item.get('Destination'), item.get('RW')) for item in info.get('Mounts', [])}
    wanted = {(m['source'], m['target'], not m['readonly']) for m in spec['mounts']}
    if (actual != wanted or len(info['Mounts']) != len(wanted)
            or any(m.get('Type') != 'bind' for m in info['Mounts']) or host.get('Binds') or host.get('VolumesFrom')):
        raise ValueError('Created child has unexpected mounts, including possible host/socket access')
    caps = lambda values: {v.upper().removeprefix('CAP_') for v in values or []}
    if (host.get('Privileged') or host.get('Devices') or host.get('DeviceRequests')
            or host.get('PidMode') not in ('', 'private') or host.get('IpcMode') != 'private'
            or host.get('NetworkMode') != spec['network'] or host.get('ReadonlyRootfs') != spec['readonly_root']
            or caps(host.get('CapDrop')) != {'ALL'} or caps(host.get('CapAdd')) != set(spec['capabilities'])
            or host.get('SecurityOpt') not in (['no-new-privileges'], ['no-new-privileges=true'])):
        raise ValueError('Created child has unexpected privileges or shared namespaces')
    env = {}
    for item in config.get('Env', []):
        name, separator, value = item.partition('=')
        if not separator or name in env:
            raise ValueError('Unexpected duplicate child environment entry')
        env[name] = value
    if set(env) - {'PATH', *spec['environment']} or any(env.get(k) != v for k, v in spec['environment'].items()):
        raise ValueError('Unexpected inherited child environment; no token/proxy/session variables are allowed')
    return cid


# Bind qualification results to the expected source and environment
def result_identity(path):
    path = directory(path)
    info = path.stat()
    return info.st_dev, info.st_ino


# Check whether the container ownership metadata matches this invocation
def ownership_matches(path, identity, uid, gid):
    if result_identity(path) != identity:
        raise ValueError('Owned results directory was replaced')
    # Represent unavailable inspection data without inventing a successful result
    def unreadable(error):
        raise error
    try:
        for parent, dirs, files in os.walk(path, followlinks=False, onerror=unreadable):
            for item in [Path(parent), *[Path(parent) / name for name in [*dirs, *files]]]:
                metadata = item.lstat()
                if (metadata.st_uid, metadata.st_gid) != (uid, gid):
                    return False
    except PermissionError:
        return False
    return True


class Docker:
    # Bind container operations to the retained command-evidence recorder
    def __init__(self, commands, executable, endpoint, client_config):
        self.commands = commands
        self.prefix = [executable, '--host', endpoint, '--config', str(client_config)]

    # Run the selected command and propagate failure to the calling workflow
    def run(self, label, args, **kwargs):
        return self.commands.text(label, [*self.prefix, *args], **kwargs)

    # Inspect the selected artifact or owned container and return its metadata
    def inspect(self, key, label):
        result = self.commands.run(label, [*self.prefix, 'container', 'inspect', '--format', '{{json .}}', key],
                                   timeout=30, allowed=(0, 1))
        if result['returncode']:
            error = (self.commands.directory / result['stderr']).read_text()
            if 'No such container:' in error or 'No such object:' in error:
                return None
            raise ValueError('Docker inspection failed; container absence is not established')
        return json.loads((self.commands.directory / result['stdout']).read_text())


# Remove only the disposable container whose ownership was verified
def cleanup_owned(docker, spec, expected_id=None):
    info = docker.inspect(expected_id or spec['name'], f'{spec["role"]}-cleanup-inspect')
    if info is None:
        return {'absent': True}
    cid = require_owned(info, spec, expected_id)
    if info['State']['Running']:
        docker.run(f'{spec["role"]}-stop', ['stop', '--time', '10', cid], timeout=30)
    current = docker.inspect(cid, f'{spec["role"]}-before-remove')
    if current is not None:
        require_owned(current, spec, cid)
        if current['State']['Running']:
            raise ValueError('Owned container did not stop; retaining it for inspection')
        docker.run(f'{spec["role"]}-remove', ['rm', cid], timeout=30)
    if docker.inspect(cid, f'{spec["role"]}-confirm-absent') is not None:
        raise ValueError('Owned container still exists after removal')
    return {'id': cid, 'absent': True}


# Run and supervise the owned qualification child process
def run_child(docker, spec, image_id, report):
    cid = None
    try:
        output = docker.run(f'{spec["role"]}-create', create_arguments(spec), timeout=60).strip()
        if not re.fullmatch(r'[0-9a-f]{64}', output):
            raise ValueError('Docker did not return a full child identity')
        cid = output
        info = docker.inspect(cid, f'{spec["role"]}-before-start')
        validate_created(info, spec, image_id)
        report['container_id'] = cid
        docker.run(f'{spec["role"]}-execute', ['start', '--attach', cid], timeout=2700 if spec['role'] == 'lifecycle' else 120)
        final = docker.inspect(cid, f'{spec["role"]}-after-exit')
        require_owned(final, spec, cid)
        report['state'] = {key: final['State'].get(key) for key in ('Status', 'Running', 'ExitCode', 'OOMKilled', 'Error')}
        if final['State']['Running'] or final['State']['ExitCode'] != 0 or final['State'].get('OOMKilled'):
            raise ValueError('Disposable child did not complete successfully')
    finally:
        report['cleanup'] = cleanup_owned(docker, spec, cid)


# Require the container engine endpoint to be local
def local_endpoint(commands, executable):
    endpoint = os.environ.get('DOCKER_HOST')
    if not endpoint:
        context = os.environ.get('DOCKER_CONTEXT') or commands.text('docker-context', [executable, 'context', 'show'], timeout=30).strip()
        if not re.fullmatch(r'[A-Za-z0-9_.-]+', context):
            raise ValueError('Unexpected Docker context name')
        endpoint = json.loads(commands.text('docker-endpoint', [executable, 'context', 'inspect', context,
            '--format', '{{json .Endpoints.docker.Host}}'], timeout=30))
    url = urllib.parse.urlsplit(endpoint)
    if url.scheme != 'unix' or url.netloc or not url.path.startswith('/') or url.query or url.fragment:
        raise ValueError('Qualification requires a local Unix Docker endpoint, never a remote engine')
    if not stat.S_ISSOCK(Path(url.path).stat().st_mode):
        raise ValueError('Docker endpoint is not a local socket')
    return endpoint


# Parse options and run the run linux qualification workflow
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    for key in ('repository', 'commit', 'build'):
        parser.add_argument('--' + key, required=True)
    parser.add_argument('--older-commit', default='')
    parser.add_argument('--older-build', default='')
    args = parser.parse_args()
    if platform.system() != 'Linux' or platform.machine().lower() not in ('x86_64', 'amd64') or Path('/.dockerenv').exists():
        parser.error('Run this launcher on the native Linux x64 host, outside any Actions job container')
    installer_inputs.identity(args.repository, args.commit, args.build, 'linux', 'manager')
    if bool(args.older_commit) != bool(args.older_build):
        parser.error('Older commit/build must both be supplied')
    if args.older_build:
        installer_inputs.identity(args.repository, args.older_commit, args.older_build, 'linux', 'manager')
    root = directory(args.root)
    driver, results = root / 'linux-container-driver', root / 'linux-container-results'
    driver.mkdir(mode=0o700)
    results.mkdir(mode=0o755)
    results.chmod(0o755)  # Nonroot qualifier user must traverse the bind root
    identity, uid, gid = result_identity(results), os.getuid(), os.getgid()
    spec = specification(root, Path(__file__).resolve().parent, args, uid, gid, uuid.uuid4().hex)
    report = {'specification': spec, 'results_identity': identity, 'owner_uid': uid, 'owner_gid': gid,
              'passed': False, 'scope': 'Owned disposable Ubuntu container DEB lifecycle; no host APT or GUI/HID'}
    write_json(driver / 'report.json', report)
    docker = None
    image_id = None
    # Stop the owned qualification operation when interrupted
    def interrupted(*_):
        raise KeyboardInterrupt('Launcher terminated')
    old_handler = signal.signal(signal.SIGTERM, interrupted)
    try:
        executable = shutil.which('docker')
        if not executable:
            raise ValueError('Install/configure Docker on the Linux runner before this optional qualification')
        # Discovery may use the caller's context, but no credentials are passed
        # into a child. An empty client config also prevents proxy-env injection
        env = {'PATH': os.environ.get('PATH', '/usr/bin:/bin'), 'HOME': os.environ.get('HOME', ''), 'LC_ALL': 'C'}
        commands = Commands(driver / 'commands', environment=env)
        endpoint = local_endpoint(commands, executable)
        config = driver / 'docker-client'
        config.mkdir(mode=0o700)
        (config / 'config.json').write_text('{}\n')
        docker = Docker(commands, executable, endpoint, config)
        docker.run('pull-pinned-image', ['pull', '--platform', 'linux/amd64', LINUX_BASELINE_IMAGE], timeout=600)
        image = json.loads(docker.run('inspect-pinned-image', ['image', 'inspect', '--format', '{{json .}}', LINUX_BASELINE_IMAGE], timeout=30))
        if (image.get('Os'), image.get('Architecture')) != ('linux', 'amd64') or not any(
                value.endswith('@' + LINUX_BASELINE_IMAGE.split('@')[1]) for value in image.get('RepoDigests', [])):
            raise ValueError('Pinned image identity/architecture differs')
        image_id = image['Id']
        report['image_id'] = image_id
        run_child(docker, spec, image_id, report)
        result = json.loads((results / 'report' / 'report.json').read_text())
        if result.get('passed') is not True or (result.get('commit'), result.get('build')) != (args.commit, args.build):
            raise ValueError('Child qualification result does not establish the selected candidate')
        report['passed'] = True
    except BaseException as error:
        report['error'] = f'{type(error).__name__}: {error}'
    finally:
        # The lifecycle child is gone before any ownership repair starts. The
        # repair child has no network, scripts, inputs, devices or host sockets
        try:
            if not ownership_matches(results, identity, uid, gid):
                if docker is None or image_id is None or not report.get('cleanup', {}).get('absent'):
                    raise ValueError('Cannot safely repair results ownership until the owned lifecycle child is absent')
                repair = specification(root, Path(__file__).resolve().parent, args, uid, gid, spec['nonce'], permissions=True)
                report['permissions_repair'] = {}
                run_child(docker, repair, image_id, report['permissions_repair'])
            if not ownership_matches(results, identity, uid, gid):
                raise ValueError('Owned results did not return to the invoking UID/GID')
            report['results_owner_restored'] = True
        except BaseException as error:
            report['passed'], report['cleanup_error'] = False, f'{type(error).__name__}: {error}'
        signal.signal(signal.SIGTERM, old_handler)
        write_json(driver / 'report.json', report)
    if not report['passed']:
        raise SystemExit('Linux qualification failed; retained logs and owned cleanup receipt explain the phase')
    print('Disposable Ubuntu package lifecycle passed; results ownership restored and owned containers removed.')


if __name__ == '__main__':
    main()
