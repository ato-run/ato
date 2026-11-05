#!/usr/bin/env python3
"""Verify source archive blobs and modes against a pinned Git tree."""
import hashlib
from pathlib import PurePosixPath
import re
import tarfile

OID = re.compile(r'^[0-9a-f]{40}$')
MAX_FILES = 20000
MAX_BLOB = 100 * 1024 * 1024
MAX_TOTAL = 500 * 1024 * 1024


def tree_identity(entries):
    root = {}
    for entry in entries:
        if entry['type'] not in ('blob', 'commit'):
            continue
        path = entry['path']
        parts = PurePosixPath(path).parts
        if not parts or path.startswith('/') or '..' in parts or '\\' in path:
            raise ValueError('Unsafe tree path')
        mode = entry['mode']
        if (entry['type'] == 'blob' and mode not in ('100644', '100755')) or (entry['type'] == 'commit' and mode != '160000'):
            raise ValueError('Unsupported Git object mode')
        oid = entry['sha']
        if not OID.fullmatch(oid):
            raise ValueError('Invalid Git object identifier')
        node = root
        for component in parts[:-1]:
            if component not in node:
                node[component] = {}
            node = node[component]
            if not isinstance(node, dict):
                raise ValueError('Overlapping tree path')
        if parts[-1] in node:
            raise ValueError('Duplicate tree path')
        node[parts[-1]] = (mode, oid)

    def encode(node):
        entries = []
        for name, value in node.items():
            raw_name = name.encode('utf-8')
            if isinstance(value, dict):
                mode, oid, key = '40000', encode(value), raw_name + b'/'
            else:
                mode, oid = value
                key = raw_name
            entries.append((key, mode.encode('ascii') + b' ' + raw_name + b'\0' + bytes.fromhex(oid)))
        payload = b''.join(value for _, value in sorted(entries))
        return hashlib.sha1(b'tree ' + str(len(payload)).encode('ascii') + b'\0' + payload).hexdigest()

    return encode(root)


def verify(archive_path, prefix, tree, expected_tree):
    if tree.get('truncated') is not False or not OID.fullmatch(expected_tree):
        raise ValueError('Invalid or truncated tree')
    entries = tree.get('tree')
    if not isinstance(entries, list) or tree_identity(entries) != expected_tree:
        raise ValueError('Git tree digest mismatch')
    expected = {item['path']: (item['mode'], item['sha']) for item in entries if item['type'] == 'blob'}
    if len(expected) > MAX_FILES:
        raise ValueError('Too many source files')
    actual = {}
    total = 0
    root = prefix.rstrip('/') + '/'
    with tarfile.open(archive_path, 'r:gz') as tar:
        for count, member in enumerate(tar, 1):
            if count > MAX_FILES * 5:
                raise ValueError('Too many archive entries')
            raw_name = member.name.rstrip('/')
            raw_parts = PurePosixPath(raw_name).parts
            if (not raw_parts or raw_name.startswith('/') or '..' in raw_parts
                    or '\\' in raw_name or not (raw_name == root.rstrip('/') or raw_name.startswith(root))):
                raise ValueError('Unsafe archive entry')
            if member.isdir():
                continue
            if not member.isfile() or not member.name.startswith(root):
                raise ValueError('Unsupported archive entry')
            name = member.name[len(root):]
            parts = PurePosixPath(name).parts
            if not parts or name.startswith('/') or '..' in parts or '\\' in name or name in actual:
                raise ValueError('Unsafe or duplicate archive entry')
            total += member.size
            if member.size > MAX_BLOB or total > MAX_TOTAL:
                raise ValueError('Source archive exceeds limit')
            data = tar.extractfile(member).read()
            digest = hashlib.sha1(b'blob ' + str(len(data)).encode('ascii') + b'\0' + data).hexdigest()
            mode = '100755' if member.mode & 0o111 else '100644'
            actual[name] = (mode, digest)
    if actual != expected:
        raise ValueError('Archive does not contain the complete Git blob tree')
    return len(actual)
