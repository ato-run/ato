import base64
import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import verify_console_packages as verifier


class ConsolePackagesTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    def fixture(self, extra=None):
        archive = self.root / 'package.tgz'
        files = {'package/package.json': json.dumps({'name': '@example/app', 'version': '1.0.0',
                                                    'license': 'AGPL-3.0-or-later'}).encode(),
                 'package/LICENSE.md': b'Synthetic notice'}
        if extra:
            files.update(extra)
        with tarfile.open(archive, 'w:gz') as out:
            for name, data in files.items():
                item = tarfile.TarInfo(name)
                item.size = len(data)
                out.addfile(item, io.BytesIO(data))
        data = archive.read_bytes()
        integrity = base64.b64encode(hashlib.sha512(data).digest()).decode()
        lock = self.root / 'lock.yaml'
        lock.write_text("  '@example/app@1.0.0':\n    resolution: {integrity: sha512-" + integrity + "}\n")
        package = ('@example/app', '1.0.0', hashlib.sha256(data).hexdigest())
        return archive, lock, package

    def test_valid_locked_package_and_notice(self):
        archive, lock, package = self.fixture()
        with patch.object(verifier, 'PACKAGES', (package,)), patch.object(verifier, 'LOCK_SHA256', hashlib.sha256(lock.read_bytes()).hexdigest()):
            result = verifier.verify(lock, [archive])
        self.assertEqual(result['packages'][0]['name'], '@example/app')
        self.assertEqual(result['packages'][0]['license_file']['sha256'], hashlib.sha256(b'Synthetic notice').hexdigest())

    def test_changed_lock_or_archive_is_rejected(self):
        archive, lock, package = self.fixture()
        with patch.object(verifier, 'PACKAGES', (package,)), patch.object(verifier, 'LOCK_SHA256', '0' * 64):
            with self.assertRaises(ValueError):
                verifier.verify(lock, [archive])
        lock_hash = hashlib.sha256(lock.read_bytes()).hexdigest()
        archive.write_bytes(archive.read_bytes() + b'x')
        with patch.object(verifier, 'PACKAGES', (package,)), patch.object(verifier, 'LOCK_SHA256', lock_hash):
            with self.assertRaises(ValueError):
                verifier.verify(lock, [archive])

    def test_pinned_archive_with_unsafe_entry_is_rejected(self):
        archive, lock, package = self.fixture({'package/../../escape': b'bad'})
        with patch.object(verifier, 'PACKAGES', (package,)), patch.object(verifier, 'LOCK_SHA256', hashlib.sha256(lock.read_bytes()).hexdigest()):
            with self.assertRaises(ValueError):
                verifier.verify(lock, [archive])


if __name__ == '__main__':
    unittest.main()
