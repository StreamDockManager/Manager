import copy
import json
import ntpath
import re
from pathlib import Path
import tempfile
import unittest
import xml.etree.ElementTree as ET

import package_metadata as m
import package_cli_native as native


class MetadataTests(unittest.TestCase):
    # Verify defaults and all platform overrides
    def test_defaults_and_all_platform_overrides(self):
        data = m.load()
        for platform in ('linux', 'macos', 'windows'):
            with self.subTest(platform=platform), tempfile.TemporaryDirectory() as temporary:
                bundle = m.tauri_override(Path(temporary), platform=platform)['bundle']
                self.assertEqual(bundle['publisher'], data['package']['publisher'])
                self.assertEqual(bundle['homepage'], data['package']['website'])
                self.assertEqual(bundle['license'], data['package']['license'])
                document = next(iter(bundle['resources']))
                self.assertEqual(json.loads((m.ROOT / 'src-tauri' / document).read_text())['product'], 'desktop')
                self.assertEqual('linux' in bundle, platform == 'linux')
                if platform == 'linux':
                    self.assertEqual(bundle['linux']['deb']['files'], bundle['linux']['rpm']['files'])
                    for path in bundle['linux']['deb']['files'].values():
                        self.assertTrue(Path(path).is_file())

    # Reproduce Tauri drive stripping and keep checkout-local resource sources on the correct drive
    def test_windows_cross_volume_resource_normalization(self):
        config = r'D:\checkout\src-tauri'
        old = r'C:\Users\runner\Temp\metadata\package-metadata.json'
        stripped = ntpath.splitdrive(old)[1]
        self.assertNotEqual(ntpath.join(config, stripped), old)
        staged = ntpath.join(config, '.package-metadata-unique', 'package-metadata.json')
        relative = ntpath.relpath(staged, config).replace('\\', '/')
        self.assertFalse(ntpath.isabs(relative))
        self.assertEqual(ntpath.normpath(ntpath.join(config, relative)), staged)
        with self.assertRaises(ValueError):
            ntpath.relpath(old, config)

    # Verify config date is release date not build date
    def test_config_date_is_release_date_not_build_date(self):
        data = m.load()
        data['package']['last_updated'] = ''
        self.assertIsNone(ET.fromstring(m.appstream(data, '0.1.0')).find('releases'))
        data['package']['last_updated'] = '2026-11-12'
        tree = ET.fromstring(m.appstream(data, '0.1.0'))
        self.assertEqual(tree.find('releases/release').attrib, {'version': '0.1.0', 'date': '2026-11-12'})

    # Verify config validation rejects bad metadata
    def test_config_validation_rejects_bad_metadata(self):
        for section, key, value in [
            ('package', 'publisher', 'bad\nMaintainer: injected'),
            ('package', 'last_updated', '2026-02-30'),
            ('package', 'last_updated', 'yesterday'),
            ('package', 'website', 'javascript:alert(1)'),
            ('package', 'website', 'https://user:secret@example.com/'),
            ('linux', 'rpm_release', '1\n%post'),
            ('linux', 'priority', 'unexpected'),
            ('desktop', 'name', ''),
            ('desktop', 'developer_id', '.'),
            ('package', 'license', 'not a license'),
            ('package', 'license', 'MIT OR'),
            ('package', 'license', '(MIT OR Apache-2.0) WITH LLVM-exception'),
        ]:
            with self.subTest(key=key, value=value):
                data = copy.deepcopy(m.load())
                data[section][key] = value
                with self.assertRaises(ValueError):
                    m.validate(data)
        data = m.load()
        data['package']['typo'] = 'ignored?'
        with self.assertRaises(ValueError):
            m.validate(data)

    # Verify xml and rpm values are escaped
    def test_xml_and_rpm_values_are_escaped(self):
        data = m.load()
        data['package']['publisher'] = 'A & B <Team> %{lua:bad}'
        tree = ET.fromstring(m.appstream(data, '0.1.0'))
        self.assertEqual(tree.find('developer/name').text, data['package']['publisher'])
        spec = native.rpm_recipe(Path('/tmp/payload'), {}, '0.1.0', data)
        self.assertIn('Vendor: A & B <Team> %%{lua:bad}', spec)
        self.assertIn('URL: ' + data['package']['website'], spec)

    # Verify override file changes native metadata and preserves notices
    def test_override_file_changes_native_metadata_and_preserves_notices(self):
        with tempfile.TemporaryDirectory(dir=m.ROOT / 'packaging') as config_dir, tempfile.TemporaryDirectory() as output:
            config = Path(config_dir) / 'metadata.toml'
            original = (m.ROOT / 'packaging/metadata.toml').read_text()
            override, count = re.subn(r'(?m)^last_updated = "[^"]*"$',
                                      'last_updated = "2026-11-12"', original)
            self.assertEqual(count, 1)
            config.write_text(override.replace('Stream Dock Manager contributors', 'Example Publisher'))
            env = {m.VARIABLE: str(config)}
            for platform in ('linux', 'windows', 'macos'):
                bundle = m.tauri_override(Path(output), env, platform)['bundle']
                self.assertEqual(bundle['publisher'], 'Example Publisher')
                document = json.loads((m.ROOT / 'src-tauri' / next(iter(bundle['resources']))).read_text())
                self.assertEqual(document['package']['last_updated'], '2026-11-12')
                self.assertNotIn('version', bundle)
                self.assertNotIn('identifier', bundle)

    # Verify paths cannot escape checkout
    def test_paths_cannot_escape_checkout(self):
        with self.assertRaises(ValueError):
            m.config_path({m.VARIABLE: '/etc/passwd'})


if __name__ == '__main__':
    unittest.main()
