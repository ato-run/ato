#!/usr/bin/env python3
"""Archive every regular blob in a pinned Git tree, without export-ignore."""
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile


COMMIT = re.compile(r'^[0-9a-f]{40}$')
MAX_BLOB = 100 * 1024 * 1024
MAX_TOTAL = 500 * 1024 * 1024
MAX_FILES = 20000


def package(repo, commit, remote_tree_path, output):
    if not COMMIT.fullmatch(commit):
        raise ValueError('Expected an exact commit SHA')
    kind = subprocess.check_output(['git', '-C', str(repo), 'cat-file', '-t', commit], text=True).strip()
    if kind != 'commit':
        raise ValueError('Expected a commit object')
    remote = json.loads(remote_tree_path.read_bytes())
    if remote.get('truncated') is not False or not isinstance(remote.get('tree'), list):
        raise ValueError('GitHub tree is missing or truncated')
    local_tree = subprocess.check_output(['git', '-C', str(repo), 'rev-parse', commit + '^{tree}'], text=True).strip()
    if remote.get('sha') != local_tree:
        raise ValueError('Git tree identity mismatch')
    expected = {}
    for item in remote['tree']:
        if item['type'] == 'commit':
            raise ValueError('Nested submodules require separate source archives')
        if item['type'] != 'blob':
            continue
        path = item['path']
        if path in expected or item['mode'] not in ('100644', '100755'):
            raise ValueError('Unsupported or duplicate Git tree entry')
        expected[path] = (item['mode'], item['sha'])
    listing = subprocess.check_output(['git', '-C', str(repo), 'ls-tree', '-r', '-z', '--full-tree', commit])
    local = {}
    for record in listing.split(b'\0'):
        if not record:
            continue
        meta, raw_name = record.split(b'\t', 1)
        mode, kind, oid = meta.decode('ascii').split(' ')
        name = raw_name.decode('utf-8')
        parsed = PurePosixPath(name)
        if (not name or parsed.is_absolute() or '..' in parsed.parts or '\\' in name
                or kind != 'blob' or mode not in ('100644', '100755') or name in local):
            raise ValueError('Unsafe or unsupported Git path')
        local[name] = (mode, oid)
    if local != expected or len(local) > MAX_FILES:
        raise ValueError('Local Git tree does not match GitHub tree')
    if output.exists() or output.is_symlink():
        raise ValueError('Output must be a new file')
    output.parent.mkdir(parents=True, exist_ok=True)
    process = subprocess.Popen(['git', '-C', str(repo), 'cat-file', '--batch'],
                               stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    created = False
    try:
        with output.open('xb') as raw:
            created = True
            with gzip.GzipFile(fileobj=raw, mode='wb', filename='', mtime=0) as compressed:
                with tarfile.open(fileobj=compressed, mode='w', format=tarfile.PAX_FORMAT) as tar:
                    total = 0
                    for name in sorted(local):
                        mode, oid = local[name]
                        process.stdin.write((oid + '\n').encode('ascii'))
                        process.stdin.flush()
                        header = process.stdout.readline().decode('ascii').strip().split(' ')
                        if len(header) != 3 or header[0] != oid or header[1] != 'blob':
                            raise ValueError('Unexpected Git object response')
                        size = int(header[2]); total += size
                        if size > MAX_BLOB or total > MAX_TOTAL:
                            raise ValueError('Source exceeds size limit')
                        data = process.stdout.read(size)
                        if len(data) != size or process.stdout.read(1) != b'\n':
                            raise ValueError('Truncated Git object')
                        if hashlib.sha1(b'blob ' + str(size).encode('ascii') + b'\0' + data).hexdigest() != oid:
                            raise ValueError('Git blob digest mismatch')
                        info = tarfile.TarInfo(name='source/' + name)
                        info.size = size
                        info.mode = 0o755 if mode == '100755' else 0o644
                        info.mtime = info.uid = info.gid = 0
                        info.uname = info.gname = ''
                        tar.addfile(info, io.BytesIO(data))
        process.stdin.close()
        if process.wait() != 0:
            raise ValueError('Git object reader failed')
    except BaseException:
        process.kill()
        process.wait()
        if created:
            output.unlink(missing_ok=True)
        raise
    finally:
        if not process.stdin.closed:
            try:
                process.stdin.close()
            except BrokenPipeError:
                pass
        process.stdout.close()
    return {'commit': commit, 'tree': local_tree, 'files': len(local),
            'archive_sha256': hashlib.sha256(output.read_bytes()).hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('repo', type=Path)
    parser.add_argument('commit')
    parser.add_argument('remote_tree', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    print(json.dumps(package(args.repo, args.commit, args.remote_tree, args.output), sort_keys=True))


if __name__ == '__main__':
    main()
