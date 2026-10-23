#!/usr/bin/env python3
"""Normalize our own built helper's Docker save to existing offline transport.

Run in this workspace's own scratch, never against an untrusted archive. Rust
still verifies every manifest/config/layer byte before Docker loads this output.
No archive or base64 image is committed.
"""
import base64, io, json, pathlib, subprocess, sys, tarfile
root=pathlib.Path(sys.argv[1]).resolve()
root.mkdir(parents=True,exist_ok=True)
prefix=sys.argv[2] if len(sys.argv)>2 else 'helper'
image=sys.argv[3] if len(sys.argv)>3 else 'ato-native-display-test:20261024'
image_id=subprocess.check_output(['docker','image','inspect',image,'--format','{{.Id}}'],text=True).strip()
raw=root/(prefix+'-original.tar')
subprocess.run(['docker','image','save',image_id,'-o',str(raw)],check=True)
with tarfile.open(raw) as source:
    entries={entry.name: source.extractfile(entry).read() for entry in source if entry.isfile()}
index=json.loads(entries['index.json'])
manifest=index['manifests'][0]
# A local build is exactly one platform manifest, not a mutable repo tag.
manifest_json=json.loads(entries['blobs/sha256/'+manifest['digest'].split(':')[1]])
assert manifest_json['schemaVersion']==2 and 'config' in manifest_json
legacy=json.loads(entries['manifest.json'])
assert len(legacy)==1
legacy[0]['RepoTags']=[]
entries['manifest.json']=json.dumps(legacy,separators=(',',':')).encode()
output=io.BytesIO()
with tarfile.open(fileobj=output,mode='w') as dest:
    for name,data in entries.items():
        item=tarfile.TarInfo(name);item.size=len(data);item.mode=0o644;dest.addfile(item,io.BytesIO(data))
archive={'image':'ato.local/native-'+prefix+'@'+manifest['digest'],'platform':'linux/amd64','bytes':base64.b64encode(output.getvalue()).decode()}
(root/(prefix+'.json')).write_text(json.dumps(archive,separators=(',',':')))
(root/(prefix+'-image')).write_text(archive['image']+'\n')
print(archive['image'])
