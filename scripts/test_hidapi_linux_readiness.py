"""Exercise the patched Linux hidraw observer with pipes, never HID hardware."""
import pathlib
import subprocess
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]


@unittest.skipUnless(sys.platform.startswith("linux"), "Linux epoll backend required")
class LinuxHidReadinessTests(unittest.TestCase):
    def test_synthetic_fd_readiness_and_retirement(self):
        subprocess.run(
            [sys.executable, str(ROOT / "scripts/prepare_patched_deps.py"), "--offline", "--check"],
            check=True,
        )
        with tempfile.TemporaryDirectory(prefix="hid-linux-readiness-") as directory:
            executable = pathlib.Path(directory) / "readiness"
            subprocess.run(
                ["cc", "-std=gnu11", "-Werror", "-pthread", "-I",
                 str(ROOT / "vendor/.cache/sources/hidapi-2.6.7/etc/hidapi/hidapi"),
                 str(ROOT / "scripts/test_hidapi_linux_readiness.c"), "-ludev", "-o", str(executable)],
                check=True,
            )
            subprocess.run([str(executable)], check=True, timeout=10)


if __name__ == "__main__":
    unittest.main()
