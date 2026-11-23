"""Build actual patched Windows backend; run synthetic events only on Windows."""
import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
COMPILER = shutil.which('gcc' if os.name == 'nt' else 'x86_64-w64-mingw32-gcc')


@unittest.skipUnless(COMPILER, 'Windows GCC toolchain is not installed')
class WindowsReadinessTests(unittest.TestCase):
    def test_backend_compiles_and_native_synthetic_events(self):
        with tempfile.TemporaryDirectory(prefix='windows-hid-readiness-') as directory:
            executable = pathlib.Path(directory) / 'readiness.exe'
            subprocess.run([COMPILER, '-Werror', '-I', str(ROOT / 'vendor/.cache/sources/hidapi-2.6.7/etc/hidapi/hidapi'), str(ROOT / 'scripts/test_hidapi_windows_readiness.c'), '-lsetupapi', '-o', str(executable)], check=True)
            if os.name == 'nt':
                subprocess.run([str(executable)], check=True, timeout=10)
            else:
                print('Cross-compiled Windows event fixture; native execution NOT performed.')


if __name__ == '__main__':
    unittest.main()
