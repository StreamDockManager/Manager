"""Build desktop and CLI installers for a selected local target."""
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile

from native_targets import NATIVE_TARGETS
from package_cli import validate_binary
from package_cli_native import FORMATS

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {target.name: (target.os, target.arch, target.rust, target.desktop_bundles)
           for target in NATIVE_TARGETS}
PRODUCTS = ('desktop', 'cli', 'all')
BUNDLE_DIRS = {'.exe': 'nsis', '.dmg': 'dmg', '.deb': 'deb', '.rpm': 'rpm'}


# Run the selected command and propagate failure to the calling workflow
def run(args, env, **kwargs):
    return subprocess.run(args, cwd=ROOT, env=env, check=True, **kwargs)


# Fail before sending the source context when Docker cannot build Linux
def linux_builder_preflight(env):
    """Fail before sending the source context when Docker cannot build Linux."""
    if not shutil.which('docker', path=env.get('PATH')):
        raise ValueError('Linux packages require Docker with Buildx. Install and start Docker Desktop, '
                         'or use a Linux Docker engine with Buildx.')
    # Buildx may use a remote builder independent of the current Docker context
    # Its nodes/platforms, not `docker info` for an unrelated engine, are authoritative.
    try:
        result = run(['docker', 'buildx', 'inspect', '--bootstrap'], env,
                     capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
        raise ValueError('Docker Buildx is not ready. Start Docker Desktop (or your selected builder), '
                         'check docker buildx inspect --bootstrap and retry.') from error
    platforms = {item.strip().rstrip('*') for line in result.stdout.splitlines()
                 if line.strip().startswith('Platforms:')
                 for item in line.split(':', 1)[1].split(',')}
    if 'linux/amd64' not in platforms:
        raise ValueError('The selected Docker builder must support linux/amd64. '
                         'On Apple Silicon, enable amd64 emulation in Docker Desktop.')


# Build native installers from the validated product payload
def build_installers(target, product, output, cargo_dir, env):
    run([sys.executable, "scripts/prepare_patched_deps.py"], env)
    osname, arch, triple, bundles = TARGETS[target]
    env = {**env, 'CARGO_TARGET_DIR': str(cargo_dir)}
    cross_windows = osname == 'windows' and sys.platform != 'win32'
    installed = run(['rustup', 'target', 'list', '--installed'], env, capture_output=True, text=True)
    if triple not in installed.stdout.splitlines():
        raise ValueError(f'Install the Rust target with rustup target add {triple}')
    if cross_windows:
        for command in ('cargo-xwin', 'clang-cl', 'lld-link', 'llvm-rc', 'makensis'):
            if not shutil.which(command, path=env.get('PATH')):
                raise ValueError(f'Windows cross builds require {command} on PATH')
        env.setdefault('XWIN_CACHE_DIR', str(ROOT / 'target/.xwin'))
    elif osname == 'windows' and product in ('cli', 'all'):
        if not env.get('STREAM_DOCK_MANAGER_NSIS_MAKENSIS') and not shutil.which('makensis.exe', path=env.get('PATH')):
            from native_installer_tools import provision
            env.update(provision(ROOT / 'target/.installer-tools'))

    release_dir = cargo_dir / triple / 'release'
    if product in ('desktop', 'all'):
        args = ['node', 'scripts/tauri.mjs', 'build', '--target', triple,
                '--config', 'src-tauri/tauri.release.conf.json', '--bundles', bundles]
        if cross_windows:
            args += ['--runner', 'cargo-xwin']
        run([*args, '--', '--locked'], {**env, 'CI': 'true'})
        binary = release_dir / ('stream-dock-manager.exe' if osname == 'windows' else 'stream-dock-manager')
        validate_binary(binary, osname, arch)
        bundle = release_dir / 'bundle'
        if osname == 'macos':
            run([sys.executable, 'scripts/finalize_desktop_dmg.py', '--dir', str(bundle)], env)
        desktop = output / 'desktop'
        desktop.mkdir(parents=True)
        for extension in FORMATS[osname]:
            packages = list((bundle / BUNDLE_DIRS[extension]).glob('*' + extension))
            if len(packages) != 1 or not packages[0].is_file() or not packages[0].stat().st_size:
                raise ValueError(f'Expected one desktop {extension} installer')
            shutil.copy2(packages[0], desktop / packages[0].name)
    if product in ('cli', 'all'):
        cargo = ['cargo', 'xwin'] if cross_windows else ['cargo']
        run([*cargo, 'build', '--locked', '--release', '--target', triple, '-p', 'stream-dock-manager-cli'], env)
        binary = release_dir / ('stream-dock-manager-cli.exe' if osname == 'windows' else 'stream-dock-manager-cli')
        run([sys.executable, 'scripts/package_cli.py', '--local', '--binary', str(binary),
             '--dir', str(output / 'cli'), '--os', osname, '--arch', arch], env)


# Build and stage the selected product target without publishing it
def package_target(target, product, env, *, linux_container=False):
    if target not in TARGETS or product not in PRODUCTS:
        raise ValueError(f'Use TARGET={"|".join(TARGETS)} and PRODUCT={"|".join(PRODUCTS)}')
    if target.startswith('macos-') and sys.platform != 'darwin':
        raise ValueError('macOS DMGs require a Mac with Xcode. Use the macOS package-build runners from other hosts.')
    if target == 'windows-amd64' and sys.platform not in ('darwin', 'linux', 'win32'):
        raise ValueError('Windows EXE packaging requires macOS, Linux or Windows')
    if linux_container:
        if target != 'linux-amd64' or sys.platform != 'linux' or platform.machine() != 'x86_64':
            raise ValueError('The Linux packaging container must run linux/amd64')
        build_installers(target, product, ROOT / 'target/local-installers', ROOT / 'target/cargo', env)
        return

    if target == 'linux-amd64':
        import package_metadata
        package_metadata.load(env)  # Fail before starting the container
        if not package_metadata.config_path(env).is_relative_to(ROOT / 'packaging'):
            raise ValueError('Docker metadata overrides must be under packaging/ to enter the build context')
        env = {**env, package_metadata.VARIABLE:
               package_metadata.config_path(env).relative_to(ROOT).as_posix()}
        linux_builder_preflight(env)

    parent = ROOT / 'target/cross' / target
    parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.build-', dir=parent) as work:
        work = Path(work)
        output = work / 'installers'
        if target == 'linux-amd64':
            docker = ['docker', 'buildx', 'build', '--platform', 'linux/amd64',
                      '--progress', 'plain',
                      '--file', 'packaging/linux/cross.Dockerfile', '--build-arg', f'PRODUCT={product}',
                      '--build-arg', f'CARGO_BUILD_JOBS={env["CARGO_BUILD_JOBS"]}',
                      '--output', f'type=local,dest={output.relative_to(ROOT).as_posix()}']
            for key in ('CARGO_NET_OFFLINE', 'STREAM_DOCK_MANAGER_SOURCE_REVISION', 'STREAM_DOCK_MANAGER_SOURCE_DIRTY',
                        'STREAM_DOCK_MANAGER_RELEASE_REPOSITORY', 'STREAM_DOCK_MANAGER_PLUGINS_RELEASE_REPOSITORY',
                        'STREAM_DOCK_MANAGER_PLUGINS_CATALOG_URL', 'STREAM_DOCK_MANAGER_PACKAGE_METADATA'):
                docker += ['--build-arg', key]
            run([*docker, '.'], env)
        else:
            build_installers(target, product, output, work / 'cargo', env)
        destination = parent / work.name.replace('.build-', 'installers-', 1)
        output.rename(destination)
    print(f'Installers: {destination}', flush=True)
