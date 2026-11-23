#!/usr/bin/env python3
"""Check source syntax/types and reject Rust warnings owned by this checkout.

Cargo's JSON diagnostics include warnings replayed for fresh/cached crates. Check
the diagnostic target's source path so workspace crates and local dependency
patches are strict, without changing compiler flags for registry dependencies.
This validates the selected host/target, not code gated to another platform.
"""
import argparse
import ast
import json
from pathlib import Path
import subprocess
import sys
import tokenize

ROOT = Path(__file__).resolve().parents[1]


# Check whether a compiler warning originates in project-owned source
def owned_warning(record, root):
    if record.get('reason') != 'compiler-message':
        return False
    if record.get('message', {}).get('level') != 'warning':
        return False
    # CI may put CARGO_HOME inside this checkout; registry/git sources are
    # still external, even when their resolved source path is under ROOT
    package = record.get('package_id', '')
    if package.startswith(('registry+', 'git+')) or '(registry+' in package or '(git+' in package:
        return False
    source = record.get('target', {}).get('src_path')
    if not isinstance(source, str) or not source:
        return False
    path = Path(source)
    if not path.is_absolute():
        path = root / path
    return path.resolve().is_relative_to(root.resolve())


# Construct the locked Cargo check invocation for the requested target
def cargo_command(*, packages=(), target=None, release=False, config=None, clippy=False):
    command = ['cargo', 'clippy' if clippy else 'check', '--locked', '--all-targets', '--message-format=json']
    if packages:
        for package in packages:
            command += ['--package', package]
    else:
        command.append('--workspace')
    if target:
        command += ['--target', target]
    if release:
        command.append('--release')
    if config:
        command += ['--config', str(config)]
    return command + (['--', '-D', 'warnings'] if clippy else [])


# Run Rust checks and reject project-owned warnings or compiler failures
def check_rust(command, root=ROOT):
    print('Checking Rust syntax/types and local warnings:', ' '.join(command), flush=True)
    warnings = 0
    # Keep Cargo progress/build-script stderr visible and stream structured stdout
    # Do not use RUSTFLAGS: registry dependencies retain their normal lint policy
    with subprocess.Popen(command, cwd=root, stdout=subprocess.PIPE, text=True,
                          encoding='utf-8', errors='replace') as process:
        for line in process.stdout:
            try:
                record = json.loads(line)
            except json.JSONDecodeError:
                print(line, end='')
                continue
            if record.get('reason') == 'compiler-message':
                message = record.get('message', {})
                rendered = message.get('rendered') or message.get('message', '')
                print(rendered, file=sys.stderr, end='' if rendered.endswith('\n') else '\n')
                warnings += owned_warning(record, root)
        status = process.wait()
    if status:
        raise subprocess.CalledProcessError(status, command)
    if warnings:
        raise ValueError(f'Rust check failed: {warnings} warning(s) in workspace/local patched crates')


# Parse Python sources without generating bytecode files
def check_python(root=ROOT):
    paths = sorted((root / 'scripts').rglob('*.py'))
    for path in paths:
        with tokenize.open(path) as source:
            ast.parse(source.read(), filename=str(path))
    print(f'Python syntax: {len(paths)} scripts passed', flush=True)


# Parse options and run the check syntax workflow
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', help='Explicit Cargo configuration file or KEY=VALUE override')
    parser.add_argument('--root', type=Path, default=ROOT, help='Rust workspace directory (with --rust-only)')
    parser.add_argument('--clippy', action='store_true', help='Run strict Clippy with the same local warning policy')
    parser.add_argument('--rust-only', action='store_true', help='Skip Python/TypeScript checks')
    parser.add_argument('--package', action='append', default=[], help='Select a Rust package (repeatable)')
    parser.add_argument('--target', help='Rust target triple; native prerequisites must be installed')
    parser.add_argument('--release', action='store_true', help='Check release-profile cfg/features; build frontend assets first')
    args = parser.parse_args()
    if args.root != ROOT and not args.rust_only:
        parser.error('--root requires --rust-only')
    if not args.rust_only:
        check_python()
        from package_metadata import load as load_package_metadata
        load_package_metadata()
        subprocess.run(['node', str(ROOT / 'node_modules/typescript/bin/tsc'), '--noEmit'],
                       cwd=ROOT, check=True)
    preparer = args.root.resolve() / 'scripts/prepare_patched_deps.py'
    if preparer.is_file():
        subprocess.run([sys.executable, str(preparer)], cwd=args.root.resolve(), check=True)
    check_rust(cargo_command(packages=args.package, target=args.target, release=args.release, config=args.config, clippy=args.clippy), root=args.root.resolve())
    print('Syntax/type and local Rust warning checks passed.', flush=True)


if __name__ == '__main__':
    try:
        main()
    except (OSError, SyntaxError, ValueError, subprocess.CalledProcessError) as error:
        print(f'check-syntax: {error}', file=sys.stderr)
        raise SystemExit(1) from error
