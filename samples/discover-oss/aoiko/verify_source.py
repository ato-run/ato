#!/usr/bin/env python3
"""Compare the bundled source archive with a pinned upstream Git tree."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import tarfile

ARCHIVE_SHA256 = 'c4c0b741cfa1588bfc5f9f2e37858f714cf60dddd67cd2d35a6c62822a176477'
TREE_SHA1 = '6437f8b86d1a35be07fbc9354bcfbc17272a3668'
OID = re.compile(r'^[0-9a-f]{40}$')
MAX_FILES = 10000
MAX_FILE_BYTES = 50 * 1024 * 1024
MAX_TOTAL_BYTES = 200 * 1024 * 1024


def tree_identity(entries):
    root = {}
    for path, (mode, oid) in entries.items():
        if mode not in ('100644', '100755') or not OID.fullmatch(oid):
            raise ValueError('Invalid Git blob metadata')
        parts = PurePosixPath(path).parts
        if not parts or path.startswith('/') or '..' in parts or '\\' in path:
            raise ValueError('Unsafe Git tree path')
        node = root
        for part in parts[:-1]:
            node = node.setdefault(part, {})
            if not isinstance(node, dict):
                raise ValueError('Overlapping Git tree paths')
        if parts[-1] in node:
            raise ValueError('Duplicate Git tree path')
        node[parts[-1]] = (mode, oid)

    def encode(node):
        result = []
        for name, value in node.items():
            raw = name.encode('utf-8')
            if isinstance(value, dict):
                mode, oid, key = '40000', encode(value), raw + b'/'
            else:
                mode, oid = value
                key = raw
            result.append((key, mode.encode() + b' ' + raw + b'\0' + bytes.fromhex(oid)))
        data = b''.join(value for _, value in sorted(result))
        return hashlib.sha1(b'tree ' + str(len(data)).encode() + b'\0' + data).hexdigest()

    return encode(root)


def verify(archive, remote_tree, archive_sha256=ARCHIVE_SHA256, tree_sha1=TREE_SHA1):
    if archive.stat().st_size > MAX_TOTAL_BYTES:
        raise ValueError('Compressed source archive exceeds size limit')
    if hashlib.sha256(archive.read_bytes()).hexdigest() != archive_sha256:
        raise ValueError('Source archive digest mismatch')
    if remote_tree.get('sha') != tree_sha1 or remote_tree.get('truncated') is not False:
        raise ValueError('Wrong or truncated upstream tree')
    expected = {}
    for item in remote_tree['tree']:
        if item['type'] == 'tree':
            continue
        if item['type'] != 'blob' or item['path'] in expected:
            raise ValueError('Unexpected or duplicate upstream object')
        expected[item['path']] = (item['mode'], item['sha'])
    if not expected or len(expected) > MAX_FILES or tree_identity(expected) != tree_sha1:
        raise ValueError('Upstream tree identity mismatch')
    actual = {}
    total = 0
    with tarfile.open(archive, 'r:gz') as tar:
        for count, item in enumerate(tar, 1):
            if count > MAX_FILES * 2:
                raise ValueError('Too many archive entries')
            name = item.name.rstrip('/')
            parts = PurePosixPath(name).parts
            if not parts or name.startswith('/') or '..' in parts or '\\' in name:
                raise ValueError('Unsafe archive path')
            if item.isdir():
                continue
            if not item.isfile() or name in actual:
                raise ValueError('Unexpected or duplicate archive entry')
            total += item.size
            if item.size > MAX_FILE_BYTES or total > MAX_TOTAL_BYTES:
                raise ValueError('Archive exceeds size limits')
            member = tar.extractfile(item)
            if member is None:
                raise ValueError('Missing archive content')
            data = member.read()
            oid = hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
            actual[name] = ('100755' if item.mode & 0o111 else '100644', oid)
    if actual != expected or tree_identity(actual) != tree_sha1:
        raise ValueError('Archive does not match the upstream Git tree')
    return len(actual)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive', type=Path)
    parser.add_argument('upstream_tree_json', type=Path)
    args = parser.parse_args()
    tree = json.loads(args.upstream_tree_json.read_text())
    print(f'Verified {verify(args.archive, tree)} files against Git tree {TREE_SHA1}')


if __name__ == '__main__':
    main()
