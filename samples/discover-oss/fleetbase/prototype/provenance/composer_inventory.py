#!/usr/bin/env python3
"""Inventory installed Composer packages in the pinned Fleetbase API image.

Run inside the read-only pinned image. This inventories metadata and notice
files; it does not prove that installed files match their upstream source.
"""
import hashlib
import json
from pathlib import Path
import re
import sys
from urllib.parse import urlsplit

INSTALLED_SHA256 = '6069cc97d9f68d64d11859cca7c3f0644ac0ef6095cef4a6ba1397b2185255dd'
LOCK_SHA256 = '20bfb94409e7aeda0dbbcedebc7866ed9856708b4874a674539733d065204dab'
NAME = re.compile(r'^[a-z0-9][a-z0-9_.-]*/[a-z0-9][a-z0-9_.-]*$')
REF = re.compile(r'^[0-9a-f]{40}$')
NOTICE_PREFIXES = ('license', 'licence', 'copying', 'notice')
MAX_NOTICE_BYTES = 1024 * 1024


def digest(data):
    return hashlib.sha256(data).hexdigest()


def read_pinned(path, expected):
    data = path.read_bytes()
    if digest(data) != expected:
        raise ValueError('Pinned input digest mismatch: ' + str(path))
    return json.loads(data)


def build_inventory(installed, locked, vendor):
    if not isinstance(installed, list) or not isinstance(locked, list):
        raise ValueError('Expected package lists')
    index = {}
    for package in locked:
        name = package.get('name')
        if not isinstance(name, str) or not NAME.fullmatch(name) or name in index:
            raise ValueError('Invalid or duplicate lock package')
        index[name] = package
    vendor = vendor.resolve(strict=True)
    result = []
    seen = set()
    for package in installed:
        name = package.get('name')
        if not isinstance(name, str) or not NAME.fullmatch(name) or name in seen:
            raise ValueError('Invalid or duplicate installed package')
        seen.add(name)
        pin = index.get(name)
        if not pin or package.get('version') != pin.get('version') or package.get('source') != pin.get('source'):
            raise ValueError('Installed package does not match lock: ' + name)
        source = package.get('source') or {}
        url = source.get('url')
        parsed = urlsplit(url) if isinstance(url, str) else None
        if (source.get('type') != 'git' or not parsed or parsed.scheme != 'https'
                or not parsed.hostname or parsed.username or parsed.password
                or parsed.query or parsed.fragment
                or not isinstance(source.get('reference'), str)
                or not REF.fullmatch(source['reference'])):
            raise ValueError('Source is not a pinned HTTPS Git repository: ' + name)
        licenses = package.get('license') or []
        if not isinstance(licenses, list) or any(not isinstance(item, str) for item in licenses):
            raise ValueError('Invalid license metadata: ' + name)
        directory = vendor.joinpath(*name.split('/'))
        if directory.is_symlink() or not directory.is_dir() or not directory.resolve().is_relative_to(vendor):
            raise ValueError('Invalid installed directory: ' + name)
        notices = []
        for path in sorted(directory.iterdir()):
            if not path.name.lower().startswith(NOTICE_PREFIXES):
                continue
            if path.is_symlink() or not path.is_file():
                raise ValueError('Invalid notice file: ' + name)
            if path.stat().st_size > MAX_NOTICE_BYTES:
                raise ValueError('Notice exceeds size limit: ' + name)
            data = path.read_bytes()
            notices.append({'path': path.name, 'sha256': digest(data), 'bytes': len(data)})
        result.append({'name': name, 'version': package['version'],
                       'source_url': source['url'], 'source_commit': source['reference'],
                       'declared_licenses': licenses, 'notices': notices})
    return sorted(result, key=lambda item: item['name'])


def main():
    api = Path('/fleetbase/api')
    installed = read_pinned(api / 'vendor/composer/installed.json', INSTALLED_SHA256)
    lock = read_pinned(api / 'composer.lock', LOCK_SHA256)
    packages = build_inventory(installed.get('packages'), lock.get('packages', []) + lock.get('packages-dev', []), api / 'vendor')
    document = {'schema': 'ato.fleetbase-composer-inventory/1',
                'image': 'fleetbase/fleetbase-api@sha256:ccd1f9d4ee93625edd9f9aa446e8655cbc2ffbe7c249387bdf52169967933572',
                'installed_sha256': INSTALLED_SHA256, 'lock_sha256': LOCK_SHA256,
                'packages': packages}
    json.dump(document, sys.stdout, indent=2, ensure_ascii=True)
    print()


if __name__ == '__main__':
    main()
