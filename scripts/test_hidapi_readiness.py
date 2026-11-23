"""Compile/run the actual patched Apple backend without accessing HID hardware."""
import pathlib
import subprocess
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]


@unittest.skipUnless(sys.platform == 'darwin', 'Apple backend requires macOS SDK')
class HidReadinessTests(unittest.TestCase):
    def test_synthetic_queue_callback_and_retirement(self):
        subprocess.run([sys.executable, str(ROOT / 'scripts/prepare_patched_deps.py'), '--offline', '--check'], check=True)
        with tempfile.TemporaryDirectory(prefix='hid-readiness-test-') as directory:
            executable = pathlib.Path(directory) / 'readiness'
            subprocess.run(['clang', '-std=c11', '-Werror', '-I', str(ROOT / 'vendor/.cache/sources/hidapi-2.6.7/etc/hidapi/hidapi'), str(ROOT / 'scripts/test_hidapi_readiness.c'), '-framework', 'IOKit', '-framework', 'CoreFoundation', '-o', str(executable)], check=True)
            symbols = subprocess.check_output(['nm', str(executable)], text=True)
            self.assertIn('_hid_darwin_set_input_ready_callback', symbols)
            subprocess.run([str(executable)], check=True, timeout=10)


if __name__ == '__main__':
    unittest.main()
