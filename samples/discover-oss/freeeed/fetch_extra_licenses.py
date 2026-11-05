#!/usr/bin/env python3
"""Fetch the exact supplemental license texts required by the notice packer."""

import concurrent.futures
import hashlib
from pathlib import Path
import sys
import urllib.request

sys.dont_write_bytecode = True
from package_dependency_notices import EXTRAS, EXTRA_URLS


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: fetch_extra_licenses.py NEW_OUTPUT_DIRECTORY")
    root = Path(sys.argv[1]).resolve()
    tmp = (Path.cwd() / ".tmp").resolve(strict=True)
    if not root.is_relative_to(tmp) or root == tmp:
        raise ValueError("output must be a new directory beneath .tmp")
    root.mkdir(parents=True, exist_ok=False)

    def fetch(name):
        with urllib.request.urlopen(EXTRA_URLS[name], timeout=60) as response:
            data = response.read(1024 * 1024 + 1)
        if len(data) > 1024 * 1024 or hashlib.sha256(data).hexdigest() != EXTRAS[name]:
            raise ValueError(f"supplemental license digest mismatch: {name}")
        (root / name).write_bytes(data)
        return name

    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        downloaded = list(pool.map(fetch, EXTRAS))
    print(f"Verified {len(downloaded)} supplemental licenses")


if __name__ == "__main__":
    main()
