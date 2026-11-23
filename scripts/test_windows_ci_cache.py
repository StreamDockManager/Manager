"""Keep Windows Rust cache compatibility and scope under regression coverage."""
import os
from pathlib import Path
import tempfile
import textwrap
import unittest
from unittest.mock import patch

WORKFLOW = (Path(__file__).resolve().parents[1] / '.github/workflows/ci.yml').read_text(encoding='utf-8')
WINDOWS_CACHE = WORKFLOW.split('            - name: Identify Windows Rust cache compatibility\n', 1)[1].split(
    '            - name: Cache Windows compiled Rust tests\n', 1
)
FINGERPRINT = textwrap.dedent(WINDOWS_CACHE[0].split('              run: |\n', 1)[1])
CACHE = WINDOWS_CACHE[1].split('            - run:', 1)[0]


class WindowsRustCacheTests(unittest.TestCase):
    def fingerprint(self, rust='rustc\nhost: x86_64-pc-windows-msvc', cargo='cargo fixture', **overrides):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'output'
            env = dict(ImageOS='win22', ImageVersion='20261122.1',
                       STREAM_DOCK_MANAGER_FULL_CHECKS='false',
                       STREAM_DOCK_MANAGER_CHECK_FOCUS='workspace', GITHUB_OUTPUT=str(output))
            env.update(overrides)
            with patch.dict(os.environ, env, clear=True), patch(
                    'subprocess.check_output', side_effect=[rust, cargo]):
                exec(FINGERPRINT, {})
            return output.read_text(encoding='utf-8').strip() if output.exists() else None

    def test_image_toolchain_profile_and_focus_change_fingerprint(self):
        baseline = self.fingerprint()
        self.assertRegex(baseline, r'^fingerprint=[a-f0-9]{64}$')
        self.assertEqual(self.fingerprint(), baseline)
        for settings in (
            {'ImageVersion': '20261122.2'}, {'ImageOS': 'win25'}, {'rust': 'different rustc'},
            {'cargo': 'different cargo'}, {'STREAM_DOCK_MANAGER_FULL_CHECKS': 'true'},
            {'STREAM_DOCK_MANAGER_CHECK_FOCUS': 'plugin-host'},
        ):
            with self.subTest(settings=settings):
                self.assertNotEqual(self.fingerprint(**settings), baseline)

    def test_nonstandard_runner_or_build_config_does_not_restore_or_save(self):
        for settings in (
            {'ImageVersion': ''}, {'CARGO_TARGET_DIR': 'elsewhere'}, {'RUSTFLAGS': '-Clto'},
        ):
            with self.subTest(settings=settings):
                self.assertIsNone(self.fingerprint(**settings))
        for guard in ("github.repository_id == '1456092478'", '!inputs.release_checks',
                      "runner.os == 'Windows'", "runner.environment == 'github-hosted'",
                      "vars.WINDOWS_RUNNER_LABELS == ''", "inputs.runner_service == 'github'"):
            self.assertIn(guard, WINDOWS_CACHE[0].split('run: |')[0])
        self.assertIn("if: steps.windows-rust-cache.outputs.fingerprint != ''", CACHE)

    def test_cache_restores_only_compiled_test_paths_and_compatible_keys(self):
        paths = CACHE.split('path: |\n', 1)[1].split('                  key:', 1)[0].split()
        self.assertEqual(paths, ['target/debug/.fingerprint', 'target/debug/build', 'target/debug/deps'])
        key = CACHE.split('key: ', 1)[1].splitlines()[0]
        restore = CACHE.split('restore-keys: |\n', 1)[1].strip()
        self.assertEqual(key, restore + '${{ github.sha }}')
        for identity in ("runner.arch", "fingerprint", "'Cargo.lock'", "'**/Cargo.toml'",
                         "'.cargo/config.toml'", "'vendor/dependencies.json'", "'patches/*.patch'"):
            self.assertIn(identity, restore)


if __name__ == '__main__':
    unittest.main()
