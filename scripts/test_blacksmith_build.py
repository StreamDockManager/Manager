import contextlib
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import blacksmith_build as build


class BlacksmithBuildTests(unittest.TestCase):
    def test_exact_native_target_and_rust_host(self):
        for target, (system, arch, triple) in build.TARGETS.items():
            with self.subTest(target=target), patch.object(build.platform, 'system', return_value=system), patch.object(build.platform, 'machine', return_value=arch), patch.object(build.subprocess, 'check_output', return_value=f'host: {triple}\n'):
                build.verify_host(target)
        with patch.object(build.platform, 'system', return_value='Darwin'), patch.object(build.platform, 'machine', return_value='x86_64'):
            with self.assertRaises(ValueError):
                build.verify_host('macos-arm64')
        with patch.object(build.platform, 'system', return_value='Linux'), patch.object(build.platform, 'machine', return_value='x86_64'), patch.object(build.subprocess, 'check_output', return_value='host: aarch64-unknown-linux-gnu\n'):
            with self.assertRaises(ValueError):
                build.verify_host('linux-amd64')

    def test_archive_contains_only_selected_binary_and_preserves_mode(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'target/release').mkdir(parents=True)
            for name in build.PRODUCTS.values():
                binary = root / 'target/release' / name
                binary.write_bytes(b'fixture')
                binary.chmod(0o755)
            for name in ['LICENSE', 'THIRD_PARTY_NOTICES.md']:
                (root / name).write_text('fixture')
            with contextlib.redirect_stdout(io.StringIO()):
                build.archive(root, 'cli', 'linux-amd64')
            with tarfile.open(root / 'artifacts/blacksmith/cli-linux-amd64.tar.gz') as archive:
                self.assertEqual(set(archive.getnames()), {'stream-dock-manager-cli', 'LICENSE', 'THIRD_PARTY_NOTICES.md', 'BUILD-README.txt'})
                self.assertEqual(archive.getmember('stream-dock-manager-cli').mode & 0o777, 0o755)
            with self.assertRaises(FileExistsError):
                build.archive(root, 'cli', 'linux-amd64')

    def test_workflow_is_manual_one_selected_job_and_artifact_only(self):
        workflow = (build.ROOT / '.github/workflows/blacksmith-build.yml').read_text()
        self.assertIn('workflow_dispatch:', workflow)
        for forbidden in ['workflow_call:', 'push:', 'pull_request:', 'matrix:', 'secrets:', 'contents: write', 'release.py', 'cargo test', 'cargo check', 'prepare-release.yml', 'macos-amd64']:
            self.assertNotIn(forbidden, workflow)
        self.assertEqual(workflow.count('uses: actions/upload-artifact@'), 1)
        self.assertIn('build --no-bundle -- --locked', workflow)
        self.assertIn("if: inputs.product == 'cli'", workflow)
        self.assertIn("if: inputs.product == 'desktop'", workflow)
        self.assertIn('persist-credentials: false', workflow)


if __name__ == '__main__':
    unittest.main()
