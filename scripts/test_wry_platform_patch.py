"""Pin the scope of the reviewed Wry compatibility-warning patch."""
import json
from pathlib import Path
import unittest


class WryPlatformPatchTests(unittest.TestCase):
    # Preserve the minimum-compatible APIs without globally disabling compiler diagnostics
    def test_patch_keeps_compatibility_and_scoped_annotations(self):
        root = Path(__file__).resolve().parents[1]
        manifest = json.loads((root/'vendor/dependencies.json').read_text())
        wry = next(item for item in manifest['dependencies'] if item['name'] == 'wry')
        entry = next(item for item in wry['patches'] if item['file'].endswith('platform-warnings.patch'))
        patch = (root/entry['file']).read_text()
        added = '\n'.join(line[1:] for line in patch.splitlines() if line.startswith('+') and not line.startswith('+++'))
        self.assertEqual(added.count('#[allow(deprecated)]'), 6)
        self.assertNotIn('#![allow', added)
        self.assertNotIn('evaluate_javascript', added)
        self.assertNotIn('cfg(feature = "v2_42")', added)
        self.assertNotIn('-          let is_display = !is_audio && !is_video;', patch)
        self.assertIn('any(debug_assertions, feature = "devtools", feature = "mac-proxy")', added)
        self.assertIn('_error.localizedDescription()', added)
