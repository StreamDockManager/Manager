"""Regression checks for the Windows OpenSSL setup shared by installer and CLI."""
from pathlib import Path
import os
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import windows_openssl_perl as setup


class WindowsOpenSSLPerlTests(unittest.TestCase):
    def test_skips_msys_and_uses_native_perl_from_path(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            msys, native = root / 'Git usr', root / 'Native Perl'
            for path in (msys, native):
                path.mkdir()
                (path / 'perl.exe').touch()
            env = {'SystemDrive': str(root), 'PATH': os.pathsep.join(map(str, [msys, native]))}
            with patch.object(setup.subprocess, 'run', side_effect=[
                subprocess.CalledProcessError(2, 'perl'), subprocess.CompletedProcess([], 0),
            ]) as run:
                self.assertEqual(setup.select_perl(env), native / 'perl.exe')
            self.assertEqual([args.args[0][0] for args in run.call_args_list],
                             [str(path / 'perl.exe') for path in (msys, native)])
            self.assertIn('-MLocale::Maketext::Simple', run.call_args.args[0])
            self.assertIn('-MIPC::Cmd', run.call_args.args[0])
            self.assertIn('MSWin32', run.call_args.args[0][-1])

    def test_broken_explicit_override_does_not_fall_back(self):
        with patch.object(setup.subprocess, 'run') as probe:
            with self.assertRaisesRegex(ValueError, 'requires native Windows Perl'):
                setup.select_perl({'OPENSSL_SRC_PERL': './perl.exe'})
            probe.assert_not_called()

    def test_exports_same_absolute_executable_for_configure_and_make(self):
        with tempfile.TemporaryDirectory() as root:
            executable = Path(root) / 'Native Perl' / 'perl.exe'
            output = Path(root) / 'env'
            with patch.object(setup.sys, 'platform', 'win32'), patch.dict(
                setup.os.environ, {'GITHUB_ENV': str(output), 'PERL': 'wrong-perl'}, clear=True
            ), patch.object(setup, 'select_perl', return_value=executable):
                setup.main()
            self.assertEqual(output.read_text().splitlines(),
                             [f'OPENSSL_SRC_PERL={executable}', f'PERL={executable}'])

    def test_windows_package_and_binary_workflows_prepare_perl_before_cargo(self):
        root = Path(__file__).resolve().parents[1] / '.github/workflows'
        for file in ['build-installers.yml', 'blacksmith-build.yml']:
            with self.subTest(file=file):
                text = (root / file).read_text(encoding='utf-8')
                marker = ("if: runner.os == 'Windows'\n"
                          "              run: python scripts/windows_openssl_perl.py")
                for build in ['cargo build --release', 'node scripts/tauri.mjs build']:
                    self.assertLess(text.index(marker), text.index(build))


if __name__ == '__main__':
    unittest.main()
