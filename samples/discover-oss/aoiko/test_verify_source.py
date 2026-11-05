import hashlib
import io
from pathlib import Path
import tarfile
import tempfile
import unittest

import verify_source as verifier


def blob(data):
    return hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()


class VerifySourceTests(unittest.TestCase):
    def test_matches_upstream_tree_and_rejects_changes(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = Path(tmp) / 'source.tar.gz'
            entries = {'source/a': ('100644', blob(b'a')),
                       'run': ('100755', blob(b'executable'))}
            with tarfile.open(archive, 'w:gz') as tar:
                for name, (mode, _) in entries.items():
                    data = b'a' if name == 'source/a' else b'executable'
                    item = tarfile.TarInfo(name)
                    item.size = len(data)
                    item.mode = int(mode, 8)
                    tar.addfile(item, io.BytesIO(data))
            pin = hashlib.sha256(archive.read_bytes()).hexdigest()
            tree_id = verifier.tree_identity(entries)
            remote = {'sha': tree_id, 'truncated': False, 'tree': [
                {'path': name, 'type': 'blob', 'mode': mode, 'sha': oid}
                for name, (mode, oid) in entries.items()
            ]}
            self.assertEqual(verifier.verify(archive, remote, pin, tree_id), 2)
            with self.assertRaisesRegex(ValueError, 'digest mismatch'):
                verifier.verify(archive, remote, '0' * 64, tree_id)
            remote['truncated'] = True
            with self.assertRaisesRegex(ValueError, 'truncated'):
                verifier.verify(archive, remote, pin, tree_id)
            remote['truncated'] = False
            remote['tree'][0]['sha'] = '0' * 40
            with self.assertRaisesRegex(ValueError, 'tree identity mismatch'):
                verifier.verify(archive, remote, pin, tree_id)

    def test_rejects_traversal_and_nonregular_entries(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = Path(tmp) / 'source.tar.gz'
            entries = {'safe': ('100644', blob(b''))}
            tree_id = verifier.tree_identity(entries)
            remote = {'sha': tree_id, 'truncated': False, 'tree': [
                {'path': 'safe', 'type': 'blob', 'mode': '100644', 'sha': blob(b'')}
            ]}
            for name, kind in [('../escape', tarfile.REGTYPE), ('link', tarfile.SYMTYPE)]:
                with tarfile.open(archive, 'w:gz') as tar:
                    item = tarfile.TarInfo(name)
                    item.type = kind
                    item.linkname = 'elsewhere'
                    tar.addfile(item, io.BytesIO(b''))
                pin = hashlib.sha256(archive.read_bytes()).hexdigest()
                with self.assertRaises(ValueError):
                    verifier.verify(archive, remote, pin, tree_id)


if __name__ == '__main__':
    unittest.main()
