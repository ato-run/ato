#!/usr/bin/env python3
"""Compare installed Fleetbase PHP files with independently pinned source archives.

Run in the pinned API image with read-only source archives. This comparison is
evidence about file bytes, not a reproducible build or a license determination.
"""
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import sys
import tarfile

ROOT_ARCHIVE_SHA256 = '720565ac99a37383801a48317d26b599505e2793a55d32497d04cd9bac957937'
ROOT_PREFIX = 'fleetbase-67b2db64c7490599aaa6c799b26d5e4de897c25c/api/'
IMAGE = 'fleetbase/fleetbase-api@sha256:ccd1f9d4ee93625edd9f9aa446e8655cbc2ffbe7c249387bdf52169967933572'
PACKAGES = {
    'packages/ai': 'fleetbase/ai',
    'packages/core-api': 'fleetbase/core-api',
    'packages/customer-portal': 'fleetbase/customer-portal-api',
    'packages/fleetops': 'fleetbase/fleetops-api',
    'packages/ledger': 'fleetbase/ledger-api',
    'packages/registry-bridge': 'fleetbase/registry-bridge',
    'packages/storefront': 'fleetbase/storefront-api',
}
MAX_FILE_SIZE = 50 * 1024 * 1024


def digest(data):
    return hashlib.sha256(data).hexdigest()


def archive_files(archive, expected_digest, prefix, ignored=()):
    data = archive.read_bytes()
    if digest(data) != expected_digest:
        raise ValueError('Source archive digest mismatch: ' + archive.name)
    result = {}
    with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as tar:
        for item in tar:
            if not item.name.startswith(prefix):
                continue
            relative = item.name[len(prefix):]
            if not relative:
                continue
            parts = PurePosixPath(relative).parts
            if not parts or any(part in ('.', '..') for part in parts) or relative.startswith('/'):
                raise ValueError('Unsafe archive path: ' + relative)
            if parts[0] in ignored or item.isdir():
                continue
            if not item.isfile() or item.size > MAX_FILE_SIZE or relative in result:
                raise ValueError('Unexpected archive entry: ' + relative)
            source = tar.extractfile(item)
            if source is None:
                raise ValueError('Missing archive content: ' + relative)
            result[relative] = digest(source.read())
    if not result:
        raise ValueError('No source files found: ' + archive.name)
    return result


def installed_files(root, ignored=()):
    if root.is_symlink() or not root.is_dir():
        raise ValueError('Invalid installed directory: ' + str(root))
    result = {}
    symlinks = []
    for current, dirs, files in os.walk(root, followlinks=False):
        base = Path(current)
        for name in list(dirs):
            path = base / name
            relative = path.relative_to(root).as_posix()
            if len(path.relative_to(root).parts) == 1 and name in ignored:
                dirs.remove(name)
            elif path.is_symlink():
                dirs.remove(name)
                symlinks.append(relative)
        for name in files:
            path = base / name
            relative = path.relative_to(root).as_posix()
            if path.is_symlink():
                symlinks.append(relative)
            elif not path.is_file() or path.stat().st_size > MAX_FILE_SIZE:
                raise ValueError('Unexpected installed file: ' + relative)
            else:
                result[relative] = digest(path.read_bytes())
    return result, sorted(symlinks)


def comparison(source, installed, symlinks):
    shared = source.keys() & installed.keys()
    return {
        'source_files': len(source), 'installed_files': len(installed),
        'matching': sum(source[path] == installed[path] for path in shared),
        'mismatches': sorted(path for path in shared if source[path] != installed[path]),
        'source_only': sorted(source.keys() - installed.keys()),
        'installed_only': sorted(installed.keys() - source.keys()),
        'installed_symlinks': symlinks,
    }


def find_archive_matches(archive, expected_digest, targets):
    data = archive.read_bytes()
    if digest(data) != expected_digest:
        raise ValueError('Source archive digest mismatch: ' + archive.name)
    matches = {name: [] for name in targets}
    with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as tar:
        for item in tar:
            if not item.isfile():
                continue
            if item.size > MAX_FILE_SIZE:
                raise ValueError('Archive file too large: ' + item.name)
            contents = tar.extractfile(item)
            if contents is None:
                raise ValueError('Missing archive content: ' + item.name)
            file_digest = digest(contents.read())
            for name, target_digest in targets.items():
                if file_digest == target_digest:
                    matches[name].append(item.name)
    return {name: sorted(paths) for name, paths in sorted(matches.items())}


def main():
    source_info = json.loads(Path('/input/fleetbase-source-trees.json').read_text())
    composer = json.loads(Path('/input/composer-inventory.json').read_text())
    submodules = {entry['path']: entry for entry in source_info['submodules']}
    installed_packages = {entry['name']: entry for entry in composer['packages']}
    api = Path('/fleetbase/api')
    root_ignore = ('vendor', '.composer', 'storage')
    root_archive = Path('/input/root.tar.gz')
    root_source = archive_files(root_archive, ROOT_ARCHIVE_SHA256, ROOT_PREFIX, root_ignore)
    root_installed, root_links = installed_files(api, root_ignore)
    report = {'schema': 'ato.fleetbase-api-source-comparison/1', 'image': IMAGE,
              'root_archive_sha256': ROOT_ARCHIVE_SHA256,
              'root': comparison(root_source, root_installed, root_links), 'packages': {}}
    extras = {path: root_installed[path] for path in report['root']['installed_only']}
    report['root']['installed_only_archive_matches'] = find_archive_matches(root_archive, ROOT_ARCHIVE_SHA256, extras)
    for path, name in sorted(PACKAGES.items()):
        pin = submodules[path]
        package = installed_packages[name]
        if pin['commit'] != package['source_commit']:
            raise ValueError('Installed source reference differs from submodule: ' + name)
        archive = Path('/input/submodules') / pin['archive']
        # GitHub codeload and locally reconstructed complete Git archives use
        # different top-level directory names.
        prefix = 'source/' if pin['method'] == 'complete-git-tree' else f"{path.split('/')[-1]}-{pin['commit']}/"
        expected = archive_files(archive, pin['archive_sha256'], prefix)
        actual, links = installed_files(api / 'vendor' / name)
        report['packages'][name] = {'commit': pin['commit'], 'archive_sha256': pin['archive_sha256'],
                                    **comparison(expected, actual, links)}
    json.dump(report, sys.stdout, indent=2, ensure_ascii=True)
    print()
    if any(item['mismatches'] or item['installed_symlinks']
           for item in [report['root'], *report['packages'].values()]):
        sys.exit(1)


if __name__ == '__main__':
    main()
