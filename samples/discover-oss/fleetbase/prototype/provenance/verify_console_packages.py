#!/usr/bin/env python3
"""Check three pinned npm source archives against Fleetbase's pinned lock."""
import argparse
import base64
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import tarfile

LOCK_SHA256 = '6b8965ebb1f995209944138c2639456c54bd0645f0d0dc680dc90ff185e912ad'
PACKAGES = (
    ('@fleetbase/ember-core', '0.3.26', 'e28ffc4dbe1daf40f93c66169259c2d08aa55093467cebd7f91fe30337e5fee1'),
    ('@fleetbase/fleetops-data', '0.2.3', 'aca236c991b3b21a6cc3e96e0c7074edaafa4bd6899dd041513f1063bcc51a0c'),
    ('@fleetbase/fleetops-engine', '0.6.75', 'f4399342a1b72e0abe80f0591c8a34d6a22f4af7bc3b12c610fc986eb9712951'),
)


def verify(lock_path, archives):
    lock = lock_path.read_bytes()
    if hashlib.sha256(lock).hexdigest() != LOCK_SHA256:
        raise ValueError('Unexpected lock file digest')
    if len(archives) != len(PACKAGES):
        raise ValueError('Expected exactly three archives')
    lock_text = lock.decode('utf-8')
    result = []
    for (name, version, expected_sha256), path in zip(PACKAGES, archives):
        if path.stat().st_size > 32 * 1024 * 1024:
            raise ValueError('Archive exceeds compressed size limit: ' + name)
        data = path.read_bytes()
        if hashlib.sha256(data).hexdigest() != expected_sha256:
            raise ValueError('Unexpected archive digest: ' + name)
        pattern = r"^  '" + re.escape(name + '@' + version) + r"':\n    resolution: \{integrity: sha512-([^}]+)\}"
        match = re.findall(pattern, lock_text, re.MULTILINE)
        actual = base64.b64encode(hashlib.sha512(data).digest()).decode('ascii')
        if len(match) != 1 or match[0] != actual:
            raise ValueError('Archive does not match lock integrity: ' + name)
        with tarfile.open(path, 'r:gz') as archive:
            members = archive.getmembers()
            if len(members) > 10000 or sum(item.size for item in members) > 200 * 1024 * 1024:
                raise ValueError('Archive exceeds inventory limits')
            names = set()
            for item in members:
                parts = PurePosixPath(item.name)
                if (parts.is_absolute() or '..' in parts.parts or not parts.parts
                        or parts.parts[0] != 'package' or item.name in names
                        or not (item.isfile() or item.isdir())):
                    raise ValueError('Unsafe archive entry: ' + name)
                names.add(item.name)
            manifest = archive.getmember('package/package.json')
            notice = archive.getmember('package/LICENSE.md')
            if not manifest.isfile() or manifest.size > 1024 * 1024 or not notice.isfile() or notice.size > 1024 * 1024:
                raise ValueError('Invalid package metadata or notice')
            metadata = json.load(archive.extractfile(manifest))
            notice_bytes = archive.extractfile(notice).read()
            if (metadata.get('name') != name or metadata.get('version') != version
                    or metadata.get('license') != 'AGPL-3.0-or-later'):
                raise ValueError('Unexpected package identity or license')
            result.append({'name': name, 'version': version, 'sha256': expected_sha256,
                           'sha512_integrity': 'sha512-' + actual,
                           'declared_license': metadata['license'],
                           'repository': metadata.get('repository'),
                           'license_file': {'path': notice.name,
                                            'sha256': hashlib.sha256(notice_bytes).hexdigest()},
                           'regular_files': sum(item.isfile() for item in members)})
    return {'schema': 'ato.fleetbase-console-packages/1', 'lock_sha256': LOCK_SHA256,
            'packages': result}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('lock', type=Path)
    parser.add_argument('archives', type=Path, nargs=3, metavar='ARCHIVE')
    args = parser.parse_args()
    print(json.dumps(verify(args.lock, args.archives), indent=2))


if __name__ == '__main__':
    main()
