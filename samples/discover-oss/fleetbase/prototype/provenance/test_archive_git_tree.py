import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

from archive_git_tree import package


class ArchiveGitTreeTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.repo = self.root / 'repo'
        self.repo.mkdir()
        self.git('init', '-q')
        (self.repo / '.gitattributes').write_text('hidden.txt export-ignore\n')
        (self.repo / 'visible.txt').write_text('visible synthetic source\n')
        (self.repo / 'hidden.txt').write_text('ignored by git archive\n')
        self.git('add', '.')
        self.tree = self.git('write-tree').strip()
        self.commit = subprocess.check_output(
            ['git', '-C', str(self.repo), 'hash-object', '-w', '-t', 'commit', '--literally', '--stdin'],
            input=f'tree {self.tree}\n\nsynthetic fixture\n'.encode(),
        ).decode().strip()
        listing = self.git('ls-tree', '-r', self.commit).splitlines()
        entries = []
        for line in listing:
            metadata, name = line.split('\t', 1)
            mode, kind, oid = metadata.split(' ')
            entries.append({'path': name, 'mode': mode, 'type': kind, 'sha': oid})
        self.tree_file = self.root / 'tree.json'
        self.tree_file.write_text(json.dumps({'sha': self.tree, 'truncated': False, 'tree': entries}))

    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.repo), *args], text=True, stderr=subprocess.PIPE)

    def test_all_blobs_including_export_ignored_files_are_archived(self):
        first = self.root / 'one.tar.gz'
        second = self.root / 'two.tar.gz'
        report = package(self.repo, self.commit, self.tree_file, first)
        package(self.repo, self.commit, self.tree_file, second)
        self.assertEqual(first.read_bytes(), second.read_bytes())
        self.assertEqual(report['files'], 3)
        with tarfile.open(first, 'r:gz') as archive:
            self.assertEqual(archive.extractfile('source/hidden.txt').read(), b'ignored by git archive\n')

    def test_mismatched_or_truncated_tree_is_refused_without_output(self):
        item = json.loads(self.tree_file.read_text())
        item['truncated'] = True
        self.tree_file.write_text(json.dumps(item))
        output = self.root / 'out.tar.gz'
        with self.assertRaises(ValueError):
            package(self.repo, self.commit, self.tree_file, output)
        self.assertFalse(output.exists())

    def test_existing_output_is_never_overwritten(self):
        output = self.root / 'out.tar.gz'
        output.write_bytes(b'keep')
        with self.assertRaises(ValueError):
            package(self.repo, self.commit, self.tree_file, output)
        self.assertEqual(output.read_bytes(), b'keep')

    def test_tree_identifier_cannot_be_used_as_a_commit(self):
        output = self.root / 'out.tar.gz'
        with self.assertRaises(ValueError):
            package(self.repo, self.tree, self.tree_file, output)
        self.assertFalse(output.exists())


if __name__ == '__main__':
    unittest.main()
