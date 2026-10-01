"""Seal downloaded wheels before any installation. Runs inside the sandbox.

No source edits, package import or arbitrary hook execution. Dependencies are
resolved by pinned pip; this stdlib operation records and verifies their bytes.
"""
import hashlib
import json
from pathlib import Path
import re
import sys
import zipfile

root = Path(sys.argv[1])
wheels = sorted(root.glob('*.whl'))
if not wheels or len(wheels) > 512:
    raise ValueError('dependency_artifact_count_limit')
artifacts = []
total = 0
for path in wheels:
    if path.is_symlink() or not path.is_file():
        raise ValueError('dependency_artifact_not_regular')
    size = path.stat().st_size
    total += size
    if size > 512 * 1024**2 or total > 2 * 1024**3:
        raise ValueError('dependency_artifact_byte_limit')
    with zipfile.ZipFile(path) as wheel:
        # Vendored dependencies may carry nested dist-info directories. Only
        # the wheel's own root dist-info identifies this artifact.
        metadata = [i for i in wheel.infolist()
                    if i.filename.endswith('.dist-info/METADATA')
                    and len(i.filename.split('/')) == 2]
        if len(metadata) != 1 or metadata[0].file_size > 1024**2:
            raise ValueError('dependency_wheel_metadata_invalid')
        fields = {}
        for line in wheel.read(metadata[0]).decode('utf-8').splitlines():
            if line.startswith(('Name: ', 'Version: ')):
                key, value = line.split(': ', 1)
                if key in fields:
                    raise ValueError('dependency_wheel_metadata_duplicate')
                fields[key] = value
    name, version = fields['Name'], fields['Version']
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]*', name) or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9.+_-]*', version):
        raise ValueError('dependency_wheel_identity_invalid')
    with path.open('rb') as stream:
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    artifacts.append(dict(file=path.name, name=name, version=version, sha256=digest, bytes=size))
if len({a['name'].lower().replace('_', '-') for a in artifacts}) != len(artifacts):
    raise ValueError('dependency_resolution_ambiguous')
manifest = dict(schema='ato.python-dependency-artifacts/1', artifacts=artifacts)
with (root / 'artifacts.json').open('x') as stream:
    json.dump(manifest, stream, sort_keys=True, separators=(',', ':'))
with (root / 'requirements.lock').open('x') as stream:
    for artifact in artifacts:
        stream.write(f"{artifact['name']}=={artifact['version']} --hash=sha256:{artifact['sha256']}\n")
print(json.dumps(dict(schema=manifest['schema'], count=len(artifacts), bytes=total)))
