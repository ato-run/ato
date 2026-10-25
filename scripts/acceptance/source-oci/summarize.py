#!/usr/bin/env python3
"""Summarize one run-all.sh output root as JSON (stdout).

usage: summarize.py OUT_ROOT
"""
import hashlib
import json
import os
import re
import sys

root = sys.argv[1]
CAUSES = [
    r"No space left on device",
    r"network is unreachable",
    r"is not allowed",
    r'"/outside\.txt": not found',
    r"wget: bad address[^\n]*",
    r"dial udp[^\n]*",
    r"lookup [^\n]*",
]


def read(path):
    with open(path, errors="replace") as handle:
        return handle.read()


def kv(path):
    out = {}
    for line in read(path).splitlines():
        if "=" in line and not line.startswith(" "):
            key, value = line.split("=", 1)
            out[key.strip()] = value.strip()
    return out


environment = read(os.path.join(root, "environment.txt")).splitlines()
provenance = json.loads(read(os.path.join(root, "build/out/source-oci-provenance.json")))
pack, export = kv(os.path.join(root, "pack.txt")), kv(os.path.join(root, "export.txt"))
runs = []
for n in (1, 2):
    receipt_path = os.path.join(root, f"run-{n}/receipt.json")
    receipt = json.loads(read(receipt_path))
    runs.append({
        "run": n,
        "fully_satisfied": receipt["fully_satisfied"],
        "derivation_ref": receipt["derivation_ref"],
        "outcomes": [o["outcome"] for o in receipt["observations"]],
        "receipt_sha256": "sha256:" + hashlib.sha256(open(receipt_path, "rb").read()).hexdigest(),
        "after": kv(os.path.join(root, f"run-{n}/result.txt")),
    })
negatives = []
for name in sorted(os.listdir(root)):
    match = re.fullmatch(r"neg-(.+)\.txt", name)
    if not match:
        continue
    lines = read(os.path.join(root, name)).splitlines()
    result = json.loads(lines[0])
    error = next((l for l in lines[1:] if l.startswith("error")), "")
    build_out = os.path.join(root, f"neg-{match.group(1)}/build.out")
    log = read(build_out) if os.path.exists(build_out) else ""
    cause = next((m.group(0) for p in CAUSES if (m := re.search(p, log))), "")
    code = re.match(r"error: (source_oci_[a-z_]+)", error)
    result.update({
        "code": code.group(1) if code else error[:80],
        "message": error[:240],
        "observed_cause": cause[:160],
    })
    negatives.append(result)
out = provenance["outputs"]
print(json.dumps({
    "environment": environment,
    "materialization": {
        "image_reference": out["image_reference"],
        "archive_sha256": out["archive_sha256"],
        "archive_bytes": out["archive_bytes"],
        "dropped_unreferenced_members": out["repack"]["dropped_unreferenced_members"],
        "build_elapsed_ms": provenance["build_elapsed_ms"],
        "source_closure_ref": provenance["inputs"]["source_closure_ref"],
        "base_images": provenance["inputs"]["base_images"],
        "session": provenance["inputs"]["builder"]["session"],
    },
    "pack": pack,
    "export": export,
    "runs": runs,
    "negatives": negatives,
}, indent=2, sort_keys=True))
