#!/usr/bin/env python3
"""Complete the pinned Beveren schedule lock without changing existing pins."""

import argparse
import hashlib
import json
from pathlib import Path
from urllib.parse import urlparse

UPSTREAM_SHA256 = "d81c53f8f3d99b4ec65b9b68df0b225830e37bc3fc1fd1032537363fde2d1e8d"
ADDITIONS_SHA256 = "27a4607d73ff57cba5da217fa76113d32b299df49e73cadfa469def62f039f95"
RESULT_SHA256 = "764c4225f02b41ac0fee3e6ef2350223ed44782f7d43c9472dabc3cd021a5783"


def checked_json(path: Path, expected: str):
    content = path.read_bytes()
    if hashlib.sha256(content).hexdigest() != expected:
        raise ValueError(f"Unexpected digest for {path.name}")
    return json.loads(content)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("upstream_lock", type=Path)
    parser.add_argument("output", type=Path, help="new output file; never overwritten")
    args = parser.parse_args()
    lock = checked_json(args.upstream_lock, UPSTREAM_SHA256)
    additions = checked_json(Path(__file__).with_name("schedule-optional-packages.json"), ADDITIONS_SHA256)
    packages = lock["packages"]
    if len(additions) != 43 or set(packages).intersection(additions):
        raise ValueError("Unexpected optional package set")
    for path, entry in additions.items():
        if (
            not path.startswith(("node_modules/@esbuild/", "node_modules/@rollup/"))
            or entry.get("optional") is not True
            or not entry.get("integrity", "").startswith("sha512-")
            or urlparse(entry.get("resolved", "")).netloc != "registry.npmjs.org"
        ):
            raise ValueError(f"Invalid optional package: {path}")
    packages.update(additions)
    # npm 11 regenerated the sorted package section and retained yaml last.
    ordered = sorted(key for key in packages if key != "node_modules/yaml")
    ordered.append("node_modules/yaml")
    lock["packages"] = {key: packages[key] for key in ordered}
    content = (json.dumps(lock, indent=2, ensure_ascii=False) + "\n").encode()
    if hashlib.sha256(content).hexdigest() != RESULT_SHA256:
        raise ValueError("Adjusted lock does not match the verified result")
    with args.output.open("xb") as output:
        output.write(content)
    print(f"Wrote verified lock: sha256:{RESULT_SHA256}")


if __name__ == "__main__":
    main()
