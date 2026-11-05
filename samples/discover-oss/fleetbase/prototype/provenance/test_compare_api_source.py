import hashlib
import io
from pathlib import Path
import tarfile
import tempfile
import unittest

import compare_api_source as compare


class SourceComparisonTests(unittest.TestCase):
    def test_archive_pin_and_comparison_detect_changed_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            archive = base / 'source.tar.gz'
            with tarfile.open(archive, 'w:gz') as tar:
                for name, data in [('source/a.php', b'one'), ('source/b.php', b'two')]:
                    info = tarfile.TarInfo(name)
                    info.size = len(data)
                    tar.addfile(info, io.BytesIO(data))
            pin = hashlib.sha256(archive.read_bytes()).hexdigest()
            source = compare.archive_files(archive, pin, 'source/')
            installed = base / 'installed'
            installed.mkdir()
            (installed / 'a.php').write_bytes(b'changed')
            (installed / 'extra.php').write_bytes(b'extra')
            actual, links = compare.installed_files(installed)
            result = compare.comparison(source, actual, links)
            self.assertEqual(result['mismatches'], ['a.php'])
            self.assertEqual(result['source_only'], ['b.php'])
            self.assertEqual(result['installed_only'], ['extra.php'])
            matches = compare.find_archive_matches(archive, pin, {
                'same': hashlib.sha256(b'two').hexdigest(),
                'missing': hashlib.sha256(b'other').hexdigest(),
            })
            self.assertEqual(matches, {'missing': [], 'same': ['source/b.php']})
            with self.assertRaisesRegex(ValueError, 'digest mismatch'):
                compare.archive_files(archive, '0' * 64, 'source/')

    def test_archive_rejects_traversal_and_symlinks(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = Path(tmp) / 'source.tar.gz'
            for name, kind in [('source/../escape', tarfile.REGTYPE),
                               ('source/link', tarfile.SYMTYPE)]:
                with tarfile.open(archive, 'w:gz') as tar:
                    info = tarfile.TarInfo(name)
                    info.type = kind
                    info.linkname = 'target'
                    tar.addfile(info, io.BytesIO(b''))
                pin = hashlib.sha256(archive.read_bytes()).hexdigest()
                with self.assertRaises(ValueError):
                    compare.archive_files(archive, pin, 'source/')

    def test_installed_symlinks_are_reported_and_not_followed(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / 'installed'
            root.mkdir()
            (root / 'file.php').write_bytes(b'content')
            (root / 'link').symlink_to(root / 'file.php')
            (root / 'external').symlink_to(Path(tmp), target_is_directory=True)
            files, links = compare.installed_files(root)
            self.assertEqual(sorted(files), ['file.php'])
            self.assertEqual(links, ['external', 'link'])


if __name__ == '__main__':
    unittest.main()
