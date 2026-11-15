"""Exercise native installer staging and disk image retry integrity."""
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import package_cli_native as native


class DiskImageTests(unittest.TestCase):
    # Preserve verification and isolate partial output across transient attempts
    def test_busy_retry_uses_fresh_output_then_verifies(self):
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory)/'cli.dmg'
            attempts = []
            def create(command, **kwargs):
                candidate = Path(command[-1]); attempts.append(candidate)
                candidate.write_bytes(b'partial' if len(attempts) == 1 else b'complete')
                return subprocess.CompletedProcess(command, 1 if len(attempts) == 1 else 0,
                    'hdiutil: create failed - Resource busy' if len(attempts) == 1 else '')
            with patch.object(native.subprocess, 'run', side_effect=create), patch.object(native, 'run') as verify, patch.object(native.time, 'sleep') as sleep:
                native.create_macos_image(Path(directory), image, '0.1.0')
                self.assertEqual(image.read_bytes(), b'complete')
                self.assertNotEqual(attempts[0], attempts[1])
                self.assertFalse(attempts[0].exists())
                verify.assert_called_once_with(['/usr/bin/hdiutil', 'verify', attempts[1]])
                sleep.assert_called_once_with(5)

    # Stop immediately for other errors and after three attempts for resource contention
    def test_failure_is_bounded_and_leaves_no_final_image(self):
        for message, count in [('hdiutil: create failed - Resource busy', 3), ('Permission denied', 1)]:
            with self.subTest(message=message), tempfile.TemporaryDirectory() as directory:
                image = Path(directory)/'cli.dmg'
                with patch.object(native.subprocess, 'run', return_value=subprocess.CompletedProcess([],1,message)) as create, patch.object(native.time,'sleep'), patch.object(native,'run') as verify:
                    with self.assertRaises(subprocess.CalledProcessError):
                        native.create_macos_image(Path(directory), image, '0.1.0')
                    self.assertEqual(create.call_count, count)
                    verify.assert_not_called()
                    self.assertFalse(image.exists())

    # Never expose a disk image that fails verification
    def test_verification_failure_is_not_retried_or_published(self):
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory)/'cli.dmg'
            with patch.object(native.subprocess,'run',return_value=subprocess.CompletedProcess([],0,'')) as create, patch.object(native,'run',side_effect=subprocess.CalledProcessError(1,['verify'])):
                with self.assertRaises(subprocess.CalledProcessError):
                    native.create_macos_image(Path(directory),image,'0.1.0')
                self.assertEqual(create.call_count,1)
                self.assertFalse(image.exists())


class LinuxInstallerTests(unittest.TestCase):
    # Analyze the staged CLI in a recognized Debian package root
    def test_debian_root_exists_before_dependency_analysis(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            root = work / 'linux-root'
            executable = root / 'usr/bin/stream-dock-manager-cli'
            executable.parent.mkdir(parents=True)
            executable.touch()
            packages = []
            def run(command, *, cwd=None, capture=False):
                if command[0] == 'dpkg-shlibdeps':
                    self.assertEqual(command, ['dpkg-shlibdeps', '-O', '-e', executable])
                    self.assertEqual(cwd, work)
                    self.assertTrue(capture)
                    self.assertTrue((root / 'DEBIAN').is_dir())
                    self.assertTrue((work / 'debian/control').is_file())
                    return 'shlibs:Depends=libc6 (>= 2.34)\n'
                if command[0] == 'dpkg-deb':
                    control = (root / 'DEBIAN/control').read_text()
                    self.assertIn('Depends: libc6 (>= 2.34), udev\n', control)
                    packages.append(command[-1])
                elif command[0] == 'rpmbuild':
                    self.assertFalse((root / 'DEBIAN').exists())
                    rpm = work / 'rpmbuild/RPMS/result.rpm'
                    rpm.touch()
                    packages.append(work / 'app.rpm')
                else:
                    self.fail(f'Unexpected packaging command: {command[0]}')
            with (patch.object(native, 'tool', side_effect=lambda _variable, fallback: fallback),
                  patch.object(native, 'linux_payload', return_value={'/usr/bin/stream-dock-manager-cli': executable}),
                  patch.object(native, 'run', side_effect=run)):
                self.assertEqual(native.write_linux(None, work, work, 'app', '0.1.0', None), packages)
            self.assertEqual(packages, [work / 'app.deb', work / 'app.rpm'])
