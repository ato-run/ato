#!/usr/bin/env python3
"""Prepare an unmodified, digest-pinned aoiko site as a static Capsule input."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath

PIN = Path(__file__).with_name('site-files.json')


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def read_site(site, inventory):
    if site.is_symlink() or not site.is_dir():
        raise ValueError('Site must be a directory, not a symlink')
    entries = inventory['files']
    expected = {}
    for entry in entries:
        name = entry['path']
        path = PurePosixPath(name)
        if (not name or path.is_absolute() or '..' in path.parts or '\\' in name
                or name == 'capsule.toml' or name in expected):
            raise ValueError('Invalid inventory path: ' + name)
        expected[name] = entry
    result = {}
    for directory, directories, files in os.walk(site, followlinks=False):
        for name in directories:
            path = Path(directory) / name
            if path.is_symlink():
                raise ValueError('Site directory is a symlink: ' + str(path))
        for name in files:
            path = Path(directory) / name
            relative = path.relative_to(site).as_posix()
            if path.is_symlink() or not path.is_file() or relative not in expected:
                raise ValueError('Unexpected site file: ' + relative)
            entry = expected[relative]
            if path.stat().st_size != entry['size']:
                raise ValueError('File size mismatch: ' + relative)
            data = path.read_bytes()
            if sha256(data) != entry['sha256']:
                raise ValueError('File digest mismatch: ' + relative)
            result[relative] = data
    if set(result) != set(expected):
        raise ValueError('Site is missing pinned files')
    return result


def manifest(files):
    text = '''schema = "ato.capsule/1"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[derive.step]]
id = "site"
use = "ato.browser@1"
op = "serve"
source = "workspace"
entry = "index.html"
spa_fallback = false

[[port]]
id = "app.http"
use = "ato.http@1"
from = "site"
'''
    observations = [('index', '/', None),
                    ('ocr-model', '/tesseract/jpn.traineddata', 'tesseract/jpn.traineddata'),
                    ('ocr-engine', '/tesseract/tesseract-core.wasm', 'tesseract/tesseract-core.wasm'),
                    ('source', '/source.tar.gz', 'source.tar.gz')]
    for name, route, file in observations:
        text += f'''
[[contract.require]]
id = "{name}"
use = "ato.contract.http@1"
port = "app.http"
method = "GET"
path = "{route}"

[contract.require.expect]
status = 200
'''
        if file:
            text += f'body_digest = "sha256:{sha256(files[file])}"\n'
    return text + '''
[[contract.require]]
id = "source-identity"
use = "ato.contract.workspace@1"
input = "workspace"

[contract.require.expect]
digest = "capture"
'''


def prepare(site, output, inventory):
    if inventory.get('schema') != 'ato.aoiko-static-site-inventory/1':
        raise ValueError('Unexpected site inventory schema')
    if output.exists() or output.is_symlink():
        raise ValueError('Output already exists')
    if output.resolve().is_relative_to(site.resolve()):
        raise ValueError('Output must be outside the source site')
    files = read_site(site, inventory)
    authored = manifest(files)
    output.mkdir(parents=True, exist_ok=False)
    for name, data in files.items():
        target = output.joinpath(*PurePosixPath(name).parts)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    (output / 'capsule.toml').write_text(authored, encoding='utf-8')
    return len(files)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('site', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    inventory = json.loads(PIN.read_text(encoding='utf-8'))
    print(f'Prepared {prepare(args.site, args.output, inventory)} unchanged site files')


if __name__ == '__main__':
    main()
