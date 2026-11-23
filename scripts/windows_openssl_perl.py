#!/usr/bin/env python3
"""Select native Windows Perl for the vendored OpenSSL build in CI."""
import os
from pathlib import Path
import subprocess
import sys


# Select only absolute paths and never let a broken explicit override fall back
def perl_candidates(env):
    explicit = env.get('OPENSSL_SRC_PERL')
    if explicit:
        yield Path(explicit)
        return
    drive = env.get('SystemDrive', 'C:') + os.sep
    yield Path(drive) / 'Strawberry' / 'perl' / 'bin' / 'perl.exe'
    for directory in env.get('PATH', '').split(os.pathsep):
        if directory:
            yield Path(directory) / 'perl.exe'


# Require native Perl and the modules OpenSSL needs before starting Cargo
def select_perl(env):
    for exe in dict.fromkeys(perl_candidates(env)):
        if not exe.is_absolute() or not exe.is_file() or any(char in str(exe) for char in '\r\n'):
            continue
        try:
            subprocess.run(
                [str(exe), '-MLocale::Maketext::Simple', '-MIPC::Cmd',
                 '-e', 'exit($^O eq "MSWin32" ? 0 : 1)'],
                env=env, check=True, capture_output=True, timeout=15,
            )
        except (OSError, subprocess.SubprocessError):
            continue
        return exe
    raise ValueError(
        'Vendored OpenSSL requires native Windows Perl with Locale::Maketext::Simple and IPC::Cmd. '
        'Install Strawberry Perl on this runner or set OPENSSL_SRC_PERL to an absolute path '
        'to a working native perl.exe. Git for Windows Perl is not supported.'
    )


# Export the executable to both Bash and PowerShell Cargo steps without editing PATH
def main():
    if sys.platform != 'win32' or not os.environ.get('GITHUB_ENV'):
        sys.exit('Run this preflight in a Windows GitHub Actions job.')
    try:
        perl = select_perl(os.environ)
    except ValueError as error:
        sys.exit(str(error))
    with open(os.environ['GITHUB_ENV'], 'a', encoding='utf-8') as output:
        print(f'OPENSSL_SRC_PERL={perl}', file=output)
        print(f'PERL={perl}', file=output)
    print(f'Vendored OpenSSL will use native Perl: {perl}')


if __name__ == '__main__':
    main()
