#!/usr/bin/env python3
"""Bootstrap must run before Cargo resolves generated patch source paths."""
import contextlib
import io
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch
import build
import check_syntax


class PatchedDependencyWiringTests(unittest.TestCase):
    # Exercise the build wrapper with mocked native build commands
    def exercise_build(self, target, fail=False):
        calls = []
        # Run the selected command and propagate failure to the calling workflow
        def run(command, **kwargs):
            calls.append(command)
            if fail:
                raise subprocess.CalledProcessError(1, command)
        with patch.object(sys, 'argv', ['build.py', target]), \
             patch.object(build, 'environment', return_value={}), \
             patch.object(build, 'commands', return_value=[['cargo', 'check']]), \
             patch.object(build.subprocess, 'run', side_effect=run):
            if fail:
                with self.assertRaises(subprocess.CalledProcessError):
                    build.main()
            else:
                build.main()
        return calls

    # Verify native build prepares before resolving cargo
    def test_native_build_prepares_before_resolving_cargo(self):
        for target in ['build', 'build-cli', 'check', 'check-syntax', 'test', 'format', 'format-check', 'desktop', 'dev', 'deps']:
            with self.subTest(target=target):
                calls = self.exercise_build(target)
                self.assertEqual(Path(calls[0][1]).name, 'prepare_patched_deps.py')
                self.assertEqual(calls[1], ['cargo', 'check'])

    # Verify preparation failure never reaches cargo
    def test_preparation_failure_never_reaches_cargo(self):
        calls = self.exercise_build('build', fail=True)
        self.assertEqual(len(calls), 1)
        self.assertEqual(Path(calls[0][1]).name, 'prepare_patched_deps.py')

    # Verify browser only build does not require rust downloads
    def test_browser_only_build_does_not_require_rust_downloads(self):
        for target in ['build-frontend', 'dev-web']:
            self.assertEqual(self.exercise_build(target), [['cargo', 'check']])

    # Verify syntax command prepares before rust check
    def test_syntax_command_prepares_before_rust_check(self):
        calls = []
        with patch.object(sys, 'argv', ['check_syntax.py', '--rust-only']), \
             patch.object(check_syntax.subprocess, 'run', side_effect=lambda command, **kwargs: calls.append('prepare')), \
             patch.object(check_syntax, 'check_rust', side_effect=lambda *args, **kwargs: calls.append('cargo')), \
             contextlib.redirect_stdout(io.StringIO()):
            check_syntax.main()
        self.assertEqual(calls, ['prepare', 'cargo'])


if __name__ == '__main__':
    unittest.main()
