"""Hermetic integrity/offline regression tests; no network or real Cargo cache."""
import concurrent.futures
import io
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import prepare_patched_deps as prep


class PreparationTests(unittest.TestCase):
    # Create isolated fixtures for PreparationTests checks
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / 'patches').mkdir()
        (self.root / 'vendor/.cache/archives').mkdir(parents=True)
        self.archive = self.root / 'vendor/.cache/archives/fixture-0.1.0.crate'
        with tarfile.open(self.archive, 'w:gz') as archive:
            for name, text in [('src/lib.rs', 'old\n'), ('Cargo.toml', '[package]\nname="fixture"\nversion="0.1.0"\n')]:
                entry = tarfile.TarInfo('fixture-0.1.0/' + name)
                entry.size = len(text.encode())
                archive.addfile(entry, io.BytesIO(text.encode()))
        self.patch = self.root / 'patches/fixture.patch'
        self.patch.write_text('--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n')
        expected = self.root / 'expected'
        prep.extract(self.archive, expected, 'fixture-0.1.0')
        (expected / 'src/lib.rs').write_text('new\n')
        self.manifest = {'version': 1, 'dependencies': [{
            'name': 'fixture', 'version': '0.1.0', 'sha256': prep.sha256(self.archive),
            'tree_sha256': prep.tree_hash(expected),
            'patches': [{'file': 'patches/fixture.patch', 'strip': 1, 'sha256': prep.sha256(self.patch)}],
        }]}
        self.save_manifest()
        self.target = self.root / 'vendor/.cache/sources/fixture-0.1.0'

    # Exercise Git checkout conversion while preserving every real manifest-pinned patch digest
    def test_windows_autocrlf_checkout_preserves_patch_bytes(self):
        source = Path(__file__).resolve().parents[1]
        repository, checkout = self.root / 'git-fixture', self.root / 'windows-checkout'
        repository.mkdir()
        checkout.mkdir()
        shutil.copyfile(source / '.gitattributes', repository / '.gitattributes')
        manifest = json.loads((source / 'vendor/dependencies.json').read_text())
        patches = [entry for dependency in manifest['dependencies'] for entry in dependency['patches']]
        for entry in patches:
            destination = repository / entry['file']
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source / entry['file'], destination)
        (repository / 'conversion-control.txt').write_bytes(b'first\nsecond\n')
        subprocess.run(['git', 'init', '--quiet', str(repository)], check=True)
        subprocess.run(['git', '-c', 'core.autocrlf=false', 'add', '.'], cwd=repository, check=True)
        subprocess.run(['git', '-c', 'core.autocrlf=true', '-c', 'core.eol=crlf', 'checkout-index', '--all', '--prefix=' + checkout.as_posix() + '/'], cwd=repository, check=True)
        self.assertEqual((checkout / 'conversion-control.txt').read_bytes(), b'first\r\nsecond\r\n')
        for entry in patches:
            with self.subTest(patch=entry['file']):
                self.assertEqual(prep.sha256(checkout / entry['file']), entry['sha256'])
                self.assertNotIn(b'\r\n', (checkout / entry['file']).read_bytes())

    # Write the dependency manifest used by the current test fixture
    def save_manifest(self):
        (self.root / 'vendor/dependencies.json').write_text(json.dumps(self.manifest))

    # Verify offline build reuses without touching source and reconstructs
    def test_offline_build_reuses_without_touching_source_and_reconstructs(self):
        with patch.object(prep.urllib.request, 'build_opener', side_effect=AssertionError('network')):
            prep.prepare(self.root, offline=True)
            source = self.target / 'src/lib.rs'
            self.assertEqual(source.read_text(), 'new\n')
            stamp = source.stat().st_mtime_ns
            prep.prepare(self.root, offline=True, check=True)
            prep.prepare(self.root, offline=True)
            self.assertEqual(source.stat().st_mtime_ns, stamp)
            shutil.rmtree(self.target)
            prep.prepare(self.root, offline=True)
            self.assertEqual(source.read_text(), 'new\n')

    # Verify corrupt tree is detected and repaired only from verified archive
    def test_corrupt_tree_is_detected_and_repaired_only_from_verified_archive(self):
        prep.prepare(self.root, offline=True)
        (self.target / 'src/lib.rs').write_text('tampered\n')
        with self.assertRaisesRegex(ValueError, 'missing or modified'):
            prep.prepare(self.root, offline=True, check=True)
        prep.prepare(self.root, offline=True)
        self.assertEqual((self.target / 'src/lib.rs').read_text(), 'new\n')
        shutil.rmtree(self.target)
        self.archive.write_bytes(b'bad archive')
        with self.assertRaisesRegex(ValueError, 'Offline cache missing or corrupt'):
            prep.prepare(self.root, offline=True)
        self.assertFalse(self.target.exists())

    # Verify patch change is rejected even with existing prepared source
    def test_patch_change_is_rejected_even_with_existing_prepared_source(self):
        prep.prepare(self.root, offline=True)
        self.patch.write_text(self.patch.read_text().replace('+new', '+unreviewed'))
        with self.assertRaisesRegex(ValueError, 'Patch checksum mismatch'):
            prep.prepare(self.root, offline=True)
        self.assertEqual((self.target / 'src/lib.rs').read_text(), 'new\n')

    # Verify context or patched tree mismatch never publishes
    def test_context_or_patched_tree_mismatch_never_publishes(self):
        self.patch.write_text(self.patch.read_text().replace('-old', '-not-the-source'))
        self.manifest['dependencies'][0]['patches'][0]['sha256'] = prep.sha256(self.patch)
        self.save_manifest()
        with self.assertRaisesRegex(ValueError, 'context mismatch'):
            prep.prepare(self.root, offline=True)
        self.assertFalse(self.target.exists())
        self.patch.write_text(self.patch.read_text().replace('-not-the-source', '-old'))
        self.manifest['dependencies'][0]['patches'][0]['sha256'] = prep.sha256(self.patch)
        self.manifest['dependencies'][0]['tree_sha256'] = '0' * 64
        self.save_manifest()
        with self.assertRaisesRegex(ValueError, 'source checksum mismatch'):
            prep.prepare(self.root, offline=True)
        self.assertFalse(self.target.exists())

    # Verify archive traversal links and duplicates are rejected
    def test_archive_traversal_links_and_duplicates_are_rejected(self):
        for kind in ('traversal', 'link', 'duplicate'):
            archive_path = self.root / (kind + '.crate')
            with tarfile.open(archive_path, 'w:gz') as archive:
                entry = tarfile.TarInfo('fixture-0.1.0/../escape' if kind == 'traversal' else 'fixture-0.1.0/file')
                if kind == 'link':
                    entry.type = tarfile.SYMTYPE
                    entry.linkname = '/tmp/outside'
                archive.addfile(entry)
                if kind == 'duplicate':
                    archive.addfile(entry)
            with self.assertRaises(ValueError):
                prep.extract(archive_path, self.root / kind, 'fixture-0.1.0')
        self.assertFalse((self.root / 'escape').exists())

    # Verify prepared tree symlinks are never followed
    def test_prepared_tree_symlinks_are_never_followed(self):
        prep.prepare(self.root, offline=True)
        source = self.target / 'src/lib.rs'
        source.unlink()
        try:
            source.symlink_to(self.patch)
        except OSError:
            self.skipTest('Host does not permit symlink fixture creation')
        with self.assertRaisesRegex(ValueError, 'non-regular'):
            prep.prepare(self.root, offline=True)
        self.assertTrue(source.is_symlink())

    # Verify parallel preparations are serialized
    def test_parallel_preparations_are_serialized(self):
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            futures = [pool.submit(prep.prepare, self.root, True) for _ in range(2)]
            for future in futures:
                future.result(timeout=10)
        self.assertEqual((self.target / 'src/lib.rs').read_text(), 'new\n')

    # Verify missing offline archive does not make network request
    def test_missing_offline_archive_does_not_make_network_request(self):
        self.archive.unlink()
        with patch.object(prep.urllib.request, 'build_opener', side_effect=AssertionError('network')):
            with self.assertRaisesRegex(ValueError, 'Offline cache missing'):
                prep.prepare(self.root, offline=True)

    # Verify download checksum failure does not publish archive
    def test_download_checksum_failure_does_not_publish_archive(self):
        self.archive.unlink()
        response = io.BytesIO(b'untrusted')
        response.geturl = lambda: 'https://static.crates.io/crates/fixture/fixture-0.1.0.crate'
        with patch.object(prep.urllib.request, 'build_opener') as factory:
            factory.return_value.open.return_value = response
            with self.assertRaisesRegex(ValueError, 'Archive checksum mismatch'):
                prep.prepare(self.root)
        self.assertFalse(self.archive.exists())
        self.assertFalse(self.target.exists())
        self.assertEqual(list(self.archive.parent.glob('.download-*')), [])

    # Verify offline environment is honored
    def test_offline_environment_is_honored(self):
        with patch.object(prep, 'prepare') as prepare, patch('sys.argv', ['prepare']), patch.dict('os.environ', {'CARGO_NET_OFFLINE': 'true'}):
            self.assertEqual(prep.main(), 0)
            prepare.assert_called_once_with(offline=True, check=False)


if __name__ == '__main__':
    unittest.main()
