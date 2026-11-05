import io
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

from verify_git_tree_archive import tree_identity, verify


class VerifyGitTreeArchiveTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        subprocess.run(['git', 'init', '-q', str(self.root / 'repo')], check=True)
        self.data = b'synthetic source\n'
        oid = subprocess.check_output(['git', '-C', str(self.root / 'repo'), 'hash-object', '-w', '--stdin'], input=self.data).decode().strip()
        entry = f'100644 blob {oid}\tfile.txt\n'.encode()
        self.tree_id = subprocess.check_output(['git', '-C', str(self.root / 'repo'), 'mktree'], input=entry).decode().strip()
        self.tree = {'truncated': False, 'tree': [{'path': 'file.txt', 'type': 'blob', 'mode': '100644', 'sha': oid}]}

    def write_archive(self, entries):
        archive = self.root / 'source.tar.gz'
        with tarfile.open(archive, 'w:gz') as out:
            for name, data in entries:
                item = tarfile.TarInfo(name)
                item.size = len(data)
                out.addfile(item, io.BytesIO(data))
        return archive

    def test_matches_git_tree_and_complete_archive(self):
        self.assertEqual(tree_identity(self.tree['tree']), self.tree_id)
        archive = self.write_archive([('root/file.txt', self.data)])
        self.assertEqual(verify(archive, 'root', self.tree, self.tree_id), 1)

    def test_missing_changed_or_unsafe_file_is_refused(self):
        for entries in [[], [('root/file.txt', b'changed')], [('root/../escape', b'x')]]:
            archive = self.write_archive(entries)
            with self.assertRaises(ValueError):
                verify(archive, 'root', self.tree, self.tree_id)

    def test_truncated_or_wrong_tree_is_refused(self):
        archive = self.write_archive([('root/file.txt', self.data)])
        with self.assertRaises(ValueError):
            verify(archive, 'root', {**self.tree, 'truncated': True}, self.tree_id)
        with self.assertRaises(ValueError):
            verify(archive, 'root', self.tree, '0' * 40)


if __name__ == '__main__':
    unittest.main()
