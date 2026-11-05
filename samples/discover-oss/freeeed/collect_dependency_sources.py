#!/usr/bin/env python3
"""Inventory exact Maven source artifacts for the experimental trimmed JAR.

The output is evidence for license review, not a conclusion of compliance.
"""

import concurrent.futures
import hashlib
import json
from pathlib import Path
import re
import sys
import urllib.error
import urllib.request
from zipfile import BadZipFile, ZipFile


MAX_BYTES = 32 * 1024 * 1024
PART = re.compile(r"^[A-Za-z0-9_.+-]+$")


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: collect_dependency_sources.py NEW_OUTPUT_DIRECTORY")
    root = Path(sys.argv[1]).resolve()
    tmp = (Path.cwd() / ".tmp").resolve(strict=True)
    if not root.is_relative_to(tmp) or root == tmp:
        raise ValueError("output must be a new directory under .tmp")
    root.mkdir(parents=True, exist_ok=False)
    deps = json.loads(Path(__file__).with_name("retained-inventory.json").read_text())["dependencies"]

    def fetch(dep):
        group, artifact, version = dep["coordinate"].split(":")
        if not all(PART.fullmatch(part) for part in (group, artifact, version)):
            raise ValueError("unsafe Maven coordinate")
        rel = "/".join([*group.split("."), artifact, version, f"{artifact}-{version}-sources.jar"])
        url = "https://repo.maven.apache.org/maven2/" + rel
        result = {"coordinate": dep["coordinate"], "binary_sha256": dep["sha256"], "url": url}
        try:
            with urllib.request.urlopen(url, timeout=60) as response:
                data = response.read(MAX_BYTES + 1)
        except urllib.error.HTTPError as error:
            if error.code != 404:
                raise
            return {**result, "status": "not_found"}
        if len(data) > MAX_BYTES:
            return {**result, "status": "too_large"}
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        try:
            with ZipFile(path) as archive:
                members = archive.namelist()
                names = [name for name in members if any(
                    token in name.rsplit("/", 1)[-1].lower() for token in ("licen", "copying", "notice")
                )]
                source_files = sum(name.lower().endswith((".java", ".kt", ".scala", ".groovy", ".c", ".h", ".py")) for name in members)
        except BadZipFile:
            path.unlink()
            return {**result, "status": "invalid_archive"}
        return {**result, "status": "available" if source_files else "no_source_files",
                "source_sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data),
                "source_files": source_files, "notice_entries": names}

    with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
        inventory = list(pool.map(fetch, deps))
    (root / "inventory.json").write_text(json.dumps(inventory, indent=2) + "\n")
    available = [entry for entry in inventory if entry["status"] == "available"]
    print(f"{len(available)}/{len(inventory)} source archives available; {sum(x['bytes'] for x in available)} bytes")


if __name__ == "__main__":
    main()
