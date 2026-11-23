"""Verify the CLI packaging source integrity gate and hashed frontend Git inputs."""
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import package_cli


class PackageSourceTests(unittest.TestCase):
    # Preserve CRLF checkout behavior except for integrity-hashed frontend inputs
    def test_checkout_keeps_acl_inputs_and_manifests_lf(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            self.git(root, 'init')
            self.git(root, 'config', 'core.autocrlf', 'true')
            (root / '.gitattributes').write_bytes(
                (package_cli.ROOT / '.gitattributes').read_bytes(),
            )
            paths = ('public/components/example.html', 'public/components/group/example.html',
                     'public/acl-manifest.json', 'public/acl-routes/acl-routes.json',
                     'README.md')
            for name in paths:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b'fixture\n')
            self.git(root, 'add', '.')
            for name in paths:
                (root / name).unlink()
            self.git(root, 'checkout-index', '-a')
            for name in paths:
                expected = b'fixture\r\n' if name == 'README.md' else b'fixture\n'
                self.assertEqual((root / name).read_bytes(), expected, name)

    # Report which status records blocked packaging without accepting dirty inputs
    def test_source_timestamp_still_rejects_untracked_and_modified_source(self):
        commit = 'a' * 40
        with patch.object(package_cli.subprocess, 'check_output', side_effect=[
            commit + '\n',
            ' M public/acl-manifest.json\n?? public/acl-routes/new.json\n',
        ]):
            with self.assertRaisesRegex(
                ValueError, r'(?s)must be a clean checkout.* M public/acl-manifest\.json.*\?\? public/acl-routes/new\.json',
            ):
                package_cli.source_timestamp(commit)

    @staticmethod
    def git(root, *args):
        return subprocess.run(
            ['git', '-C', str(root), *args], check=True, capture_output=True,
        )


if __name__ == '__main__':
    unittest.main()
