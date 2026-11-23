"""Run disposable integration tests without using the developer's credential store."""
from contextlib import contextmanager
import csv
import os
from pathlib import Path
import secrets
import subprocess
import sys
import tempfile


@contextmanager
def test_environment():
    variable = 'STREAM_DOCK_MANAGER_INSTALLATION_KEY_FILE'
    if variable in os.environ:
        yield os.environ
        return
    with tempfile.TemporaryDirectory(prefix='manager-test-installation-') as directory:
        key = Path(directory) / 'key'
        with os.fdopen(os.open(key, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as output:
            if os.name == 'nt':
                identity = subprocess.check_output(['whoami', '/user', '/fo', 'csv', '/nh'], text=True)
                sid = next(csv.reader(identity.splitlines()))[1]
                subprocess.run(['icacls', str(key), '/setowner', '*' + sid], check=True, stdout=subprocess.DEVNULL)
                subprocess.run(['icacls', str(key), '/inheritance:r', '/grant:r', '*' + sid + ':F'], check=True, stdout=subprocess.DEVNULL)
            output.write(secrets.token_hex(32))
        yield {**os.environ, variable: str(key)}


if __name__ == '__main__':
    with test_environment() as env:
        sys.exit(subprocess.run(sys.argv[1:], env=env).returncode)
