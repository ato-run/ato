import tempfile
from pathlib import Path
import unittest

import prepare_static as aoiko


class PrepareStaticTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.site = self.root / 'site'
        self.site.mkdir()
        self.contents = {'index.html': b'<html>sample</html>', 'source.tar.gz': b'source',
                         'tesseract/jpn.traineddata': b'model',
                         'tesseract/tesseract-core.wasm': b'wasm'}
        for name, data in self.contents.items():
            path = self.site / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        self.inventory = {'schema': 'ato.aoiko-static-site-inventory/1', 'files': [
            {'path': name, 'size': len(data), 'sha256': aoiko.sha256(data)}
            for name, data in self.contents.items()
        ]}

    def test_prepares_exact_files_and_declares_asset_digests(self):
        output = self.root / 'output'
        self.assertEqual(aoiko.prepare(self.site, output, self.inventory), 4)
        for name, data in self.contents.items():
            self.assertEqual((output / name).read_bytes(), data)
        manifest = (output / 'capsule.toml').read_text()
        self.assertIn(aoiko.sha256(b'model'), manifest)
        self.assertIn('use = "ato.browser@1"', manifest)
        with self.assertRaisesRegex(ValueError, 'already exists'):
            aoiko.prepare(self.site, output, self.inventory)

    def test_rejects_altered_missing_and_extra_files_before_writing(self):
        output = self.root / 'output'
        path = self.site / 'index.html'
        original = path.read_bytes()
        path.write_bytes(b'x' * len(original))
        with self.assertRaisesRegex(ValueError, 'digest mismatch'):
            aoiko.prepare(self.site, output, self.inventory)
        self.assertFalse(output.exists())
        path.unlink()
        with self.assertRaisesRegex(ValueError, 'missing'):
            aoiko.prepare(self.site, output, self.inventory)
        path.write_bytes(original)
        (self.site / 'extra').write_bytes(b'extra')
        with self.assertRaisesRegex(ValueError, 'Unexpected site file'):
            aoiko.prepare(self.site, output, self.inventory)
        self.assertFalse(output.exists())

    def test_rejects_symlink_and_output_inside_source(self):
        path = self.site / 'index.html'
        path.unlink()
        path.symlink_to(self.site / 'source.tar.gz')
        with self.assertRaises(ValueError):
            aoiko.prepare(self.site, self.root / 'output', self.inventory)
        with self.assertRaisesRegex(ValueError, 'outside'):
            aoiko.prepare(self.site, self.site / 'output', self.inventory)


if __name__ == '__main__':
    unittest.main()
