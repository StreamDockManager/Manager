#!/usr/bin/env python3
"""Standalone native builds using this checkout's public workspace defaults."""
import argparse
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[1]
TARGETS = ['help', 'deps', 'build', 'build-cli', 'build-frontend', 'package', 'package-target',
           'check', 'check-syntax', 'test', 'format', 'format-check', 'desktop', 'dev', 'dev-web']


# Resolve build settings without changing persistent user configuration
def environment():
    env = os.environ.copy()
    path = Path(env.get('WORKSPACE_CONFIG') or ROOT / 'workspace.toml')
    with path.open('rb') as source:
        config = tomllib.load(source)
    if any(not isinstance(config.get(section, {}), dict) for section in ['manager', 'plugins']):
        raise ValueError('workspace.toml manager and plugins settings must be TOML tables')
    for variable, section, key in [
        ('STREAM_DOCK_MANAGER_RELEASE_REPOSITORY', 'manager', 'release_repository'),
        ('STREAM_DOCK_MANAGER_PLUGINS_RELEASE_REPOSITORY', 'plugins', 'release_repository'),
        ('STREAM_DOCK_MANAGER_PLUGINS_CATALOG_URL', 'plugins', 'catalog_url'),
    ]:
        value = env.get(variable, config.get(section, {}).get(key, ''))
        if not isinstance(value, str) or any(ord(c) < 32 or ord(c) == 127 for c in value):
            raise ValueError(f'{section}.{key} must be a string without control characters')
        if key == 'release_repository' and value and not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', value):
            raise ValueError(f'{section}.{key} must use GitHub owner/repository syntax')
        if key == 'catalog_url' and value:
            url = urlsplit(value)
            if (url.scheme != 'https' or not url.hostname or url.username is not None
                    or url.password is not None or url.query or url.fragment
                    or any(c.isspace() for c in value) or '\\' in value or '%' in value
                    or url.path.endswith('/')):
                raise ValueError('plugins.catalog_url must be a complete public HTTPS catalog URL')
            _ = url.port
        env[variable] = value
    jobs = int(env.get('WORKSPACE_JOBS') or min(4, os.cpu_count() or 1))
    if not 1 <= jobs <= 128:
        raise ValueError('WORKSPACE_JOBS must be between 1 and 128')
    env.setdefault('CARGO_BUILD_JOBS', str(jobs))
    for variable, args in [
        ('STREAM_DOCK_MANAGER_SOURCE_REVISION', ['rev-parse', 'HEAD']),
        ('STREAM_DOCK_MANAGER_SOURCE_DIRTY', ['status', '--porcelain']),
    ]:
        if variable in env:
            continue
        try:
            value = subprocess.check_output(['git', *args], cwd=ROOT, stderr=subprocess.DEVNULL, text=True).strip()
            env[variable] = str(bool(value)).lower() if variable.endswith('DIRTY') else value
        except (OSError, subprocess.CalledProcessError):
            env[variable] = 'unknown'
    return env


# Build the ordered command list for the selected product target
def commands(action, system, env):
    npm = 'npm.cmd' if system == 'win32' else 'npm'
    if action == 'desktop':
        action = 'dev'
    if action == 'package':
        bundles = {'darwin': 'app,dmg', 'win32': 'nsis', 'linux': 'deb,rpm'}.get(system)
        if bundles is None:
            raise ValueError('Native packaging supports macOS, Windows and Linux')
        result = [['node', 'scripts/tauri.mjs', 'build', '--config',
                   'src-tauri/tauri.release.conf.json', '--bundles', bundles]]
        if system == 'darwin':
            directory = Path(env.get('CARGO_TARGET_DIR') or 'target') / 'release/bundle'
            result.append([sys.executable, 'scripts/finalize_desktop_dmg.py', '--dir', str(directory)])
        return result
    return {
        'deps': [[npm, 'ci']],
        'build': [[npm, 'run', 'build:native'], ['cargo', 'build', '--locked', '-p', 'stream-dock-manager']],
        'build-cli': [['cargo', 'build', '--locked', '--release', '-p', 'stream-dock-manager-cli']],
        'build-frontend': [[npm, 'run', 'build']],
        'check': [[npm, 'run', 'check']],
        'check-syntax': [[sys.executable, 'scripts/check_syntax.py']],
        'test': [['cargo', 'test', '--locked', '--workspace']],
        'format': [[npm, 'run', 'format']],
        'format-check': [[npm, 'run', 'format:check']],
        'dev': [[npm, 'run', 'desktop']],
        'dev-web': [[npm, 'run', 'dev']],
    }[action]


# Parse options and run the build workflow
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('target', nargs='?', choices=TARGETS, default='help')
    parser.add_argument('--linux-container', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    action = args.target
    if action == 'help':
        print('Standalone Stream Dock Manager (GNU Make, Python 3.11+, Rust; Node 26/npm 12 for desktop).\n'
              'deps: install frontend dependencies; build: native debug desktop + production frontend.\n'
              'build-cli: native release CLI, without Node/frontend; build-frontend: production web assets.\n'
              'package: native desktop installers using the existing Tauri integrity policy.\n'
              'make build-macos ARCH=arm64|amd64|both: selected macOS desktop/CLI installers (default both).\n'
              'make build-linux: Docker-based Linux x64 desktop and CLI packages from any host.\n'
              'package-target TARGET=windows-amd64|macos-amd64|macos-arm64|linux-amd64 PRODUCT=desktop|cli|all.\n'
              'desktop (alias: dev): native development app with hot reload; Rust compiles as needed.\n'
              'check | check-syntax (Python/TypeScript/Rust; local Rust warnings fail) | test (native Rust only) | format | format-check | dev-web\n'
              'Defaults: this repository/workspace.toml; WORKSPACE_CONFIG selects another file.\n'
              'Explicit STREAM_DOCK_MANAGER_* environment settings override public defaults.\n'
              'WORKSPACE_JOBS bounds build parallelism. Install native Tauri prerequisites first.')
        return
    env = environment()
    if action not in {"build-frontend", "dev-web", "package-target"}:
        subprocess.run([sys.executable, str(ROOT / "scripts/prepare_patched_deps.py")], cwd=ROOT, env=env, check=True)
    if action == 'package-target':
        from package_target import package_target
        package_target(env.get('TARGET', ''), env.get('PRODUCT') or 'desktop', env,
                       linux_container=args.linux_container)
        return
    for args in commands(action, sys.platform, env):
        subprocess.run(args, cwd=ROOT, env=env, check=True)


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr)
        sys.exit(1)
