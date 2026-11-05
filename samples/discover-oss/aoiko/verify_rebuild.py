#!/usr/bin/env python3
"""Verify a pinned aoiko checkout's web build against the candidate site."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

from prepare_static import PIN, read_site

COMMIT = '3c135266c87747ad86a29d72a038cbe5abff9205'
TREE = '6437f8b86d1a35be07fbc9354bcfbc17272a3668'
LOCK_SHA256 = '83551a51a4a3bd3d6869fc0e3fa77f1fc7c9ad6d9529ff3d92d9cdd880e61518'
EXTRA_FILES = {'LICENSE.txt', 'source.tar.gz'}


def git(checkout, *args):
    return subprocess.check_output(['git', '-C', str(checkout), *args])


def verify(site, checkout, inventory):
    if git(checkout, 'rev-parse', 'HEAD').decode().strip() != COMMIT:
        raise ValueError('Wrong upstream commit')
    if git(checkout, 'rev-parse', 'HEAD^{tree}').decode().strip() != TREE:
        raise ValueError('Wrong upstream Git tree')
    if git(checkout, 'status', '--porcelain', '--untracked-files=no').strip():
        raise ValueError('Upstream checkout has tracked modifications')
    if hashlib.sha256((checkout / 'package-lock.json').read_bytes()).hexdigest() != LOCK_SHA256:
        raise ValueError('Package lock digest mismatch')
    original = read_site(site, inventory)
    if original['LICENSE.txt'] != git(checkout, 'show', 'HEAD:LICENSE'):
        raise ValueError('Candidate license does not match upstream')
    build_inventory = {'files': [entry for entry in inventory['files']
                                  if entry['path'] not in EXTRA_FILES]}
    built = read_site(checkout / 'dist', build_inventory)
    if any(built[path] != original[path] for path in built):
        raise ValueError('Build output differs from candidate')
    return len(built)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('site', type=Path)
    parser.add_argument('checkout', type=Path)
    args = parser.parse_args()
    inventory = json.loads(PIN.read_text(encoding='utf-8'))
    print(f'Verified {verify(args.site, args.checkout, inventory)} generated files and upstream LICENSE')


if __name__ == '__main__':
    main()
