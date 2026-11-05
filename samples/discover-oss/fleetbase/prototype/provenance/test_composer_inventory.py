import copy
import json
import tempfile
from pathlib import Path
import unittest

from composer_inventory import build_inventory, read_pinned


class ComposerInventoryTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.vendor = Path(self.temporary.name) / 'vendor'
        package = self.vendor / 'example' / 'library'
        package.mkdir(parents=True)
        (package / 'LICENSE').write_text('Synthetic notice')
        self.package = {'name': 'example/library', 'version': '1.0.0',
                        'source': {'type': 'git', 'url': 'https://example.com/repo.git',
                                   'reference': 'a' * 40}, 'license': ['MIT']}

    def test_valid_inventory_retains_source_and_actual_notice_digest(self):
        result = build_inventory([self.package], [self.package], self.vendor)
        self.assertEqual(result[0]['source_commit'], 'a' * 40)
        self.assertEqual(result[0]['notices'][0]['bytes'], len(b'Synthetic notice'))
        self.assertEqual(len(result[0]['notices'][0]['sha256']), 64)

    def test_missing_notice_is_reported_not_invented(self):
        (self.vendor / 'example/library/LICENSE').unlink()
        self.assertEqual(build_inventory([self.package], [self.package], self.vendor)[0]['notices'], [])

    def test_mismatch_duplicate_and_unpinned_sources_fail(self):
        changed = copy.deepcopy(self.package)
        changed['version'] = '2.0.0'
        with self.assertRaises(ValueError):
            build_inventory([changed], [self.package], self.vendor)
        with self.assertRaises(ValueError):
            build_inventory([self.package, self.package], [self.package], self.vendor)
        bad = copy.deepcopy(self.package)
        bad['source']['reference'] = 'main'
        with self.assertRaises(ValueError):
            build_inventory([bad], [bad], self.vendor)
        bad = copy.deepcopy(self.package)
        bad['source']['url'] = 'https://user:password@example.com/repo.git'
        with self.assertRaises(ValueError):
            build_inventory([bad], [bad], self.vendor)

    def test_notice_symlink_is_rejected(self):
        notice = self.vendor / 'example/library/LICENSE'
        notice.unlink()
        notice.symlink_to(Path(self.temporary.name) / 'outside')
        with self.assertRaises(ValueError):
            build_inventory([self.package], [self.package], self.vendor)

    def test_input_hash_is_enforced(self):
        file = Path(self.temporary.name) / 'input.json'
        file.write_text(json.dumps({'packages': []}))
        with self.assertRaises(ValueError):
            read_pinned(file, '0' * 64)


class RecordedEvidenceTest(unittest.TestCase):
    def test_supplements_are_pinned_to_installed_sources(self):
        import hashlib
        root = Path(__file__).parent
        inventory = json.loads((root / 'composer-inventory.json').read_text())
        supplements = json.loads((root / 'supplemental-notices.json').read_text())
        packages = {item['name']: item for item in inventory['packages']}
        missing = {item['name'] for item in packages.values() if not item['notices']}
        for item in supplements['notices']:
            package = packages[item['package']]
            self.assertIn(item['package'], missing)
            prefix = package['source_url'].removesuffix('.git') + '/blob/' + package['source_commit'] + '/'
            self.assertTrue(item['source_url'].startswith(prefix))
            self.assertEqual(hashlib.sha256((root / item['path']).read_bytes()).hexdigest(), item['sha256'])
        self.assertEqual(missing - {item['package'] for item in supplements['notices']},
                         set(supplements['unresolved_missing_notices']))


if __name__ == '__main__':
    unittest.main()
