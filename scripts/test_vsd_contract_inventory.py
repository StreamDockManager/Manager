"""Factual VSD source extraction regressions, with no code execution or network."""
import json
from pathlib import Path
import tempfile
import unittest

from vsd_contract_inventory import DocumentationIdentifiers, documentation_snapshot, inventory


class VsdInventoryTests(unittest.TestCase):
    # Verify documentation parser retains identifiers not prose
    def test_documentation_parser_retains_identifiers_not_prose(self):
        parser = DocumentationIdentifiers()
        parser.feed('<h2>Actions</h2><table><tr><td>UUID</td><td>Required</td><td>Vendor prose</td></tr>'
                    '<tr><td>FontSize</td><td>Optional</td><td>More prose</td></tr></table>')
        self.assertEqual(parser.members, [{'section': 'Actions', 'name': 'UUID', 'requiredness': 'Required'},
                                          {'section': 'Actions', 'name': 'FontSize', 'requiredness': 'Optional'}])
        self.assertNotIn('Vendor prose', json.dumps(parser.members))

    # Verify snapshot hashes source and rejects empty or challenge pages
    def test_snapshot_hashes_source_and_rejects_empty_or_challenge_pages(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for page in ('manifest', 'events-sent', 'events-received'):
                (root / (page + '.html')).write_text('<h2>setTitle</h2><table><tr><td>Title</td><td>Required</td></tr></table>')
            snapshot = documentation_snapshot(root, '2026-11-09')
            self.assertEqual(len(snapshot['pages']), 3)
            self.assertTrue(all(len(page['source_sha256']) == 64 for page in snapshot['pages']))
            self.assertEqual(snapshot['pages'][1]['events'], ['setTitle'])
            (root / 'manifest.html').write_text('<h1>Please enable JavaScript</h1>')
            with self.assertRaisesRegex(ValueError, 'Missing documented identifiers'):
                documentation_snapshot(root, '2026-11-09')

    # Verify pinned inventory covers docs core commands and nested manifest fields
    def test_pinned_inventory_covers_docs_core_commands_and_nested_manifest_fields(self):
        rows = inventory()
        ids = {row['id'] for row in rows}
        self.assertEqual(len(ids), len(rows))
        for name in ('vsd.command.setTitle', 'vsd.command.switchToProfile', 'vsd.command.setFeedback',
                     'vsd.event.dialRotate', 'vsd.event.touchTap', 'vsd.manifest-field.Nodejs.Version',
                     'vsd.manifest-field.Actions[].States[].FontSize'):
            self.assertIn(name, ids)
        self.assertTrue(all(row['status'] == 'unverified' for row in rows))
        set_title = next(row for row in rows if row['id'] == 'vsd.command.setTitle')
        self.assertTrue(set_title['additional_sources'])
        self.assertEqual(set_title['source'], 'vendor/vsd/documentation-facts.json')


if __name__ == '__main__':
    unittest.main()
