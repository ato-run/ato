#!/usr/bin/env python3
"""Fetch the four hash-pinned binary replacements for the TXT preview."""

import concurrent.futures
import hashlib
from pathlib import Path
import sys
import urllib.request


ITEMS = {
    "sqlite-jdbc-3.41.2.2.jar": ("org/xerial/sqlite-jdbc/3.41.2.2", "0cdab410947e04b6743df99cf1543267ddd107357d6f76948d145be590fd497d"),
    "guava-33.8.0-jre.jar": ("com/google/guava/guava/33.8.0-jre", "97b79ec3f2c0a6313a0c2df51ce1abdf8907e6531803467b81efcaaf99b35161"),
    "failureaccess-1.0.3.jar": ("com/google/guava/failureaccess/1.0.3", "cbfc3906b19b8f55dd7cfd6dfe0aa4532e834250d7f080bd8d211a3e246b59cb"),
    "commons-io-2.14.0.jar": ("commons-io/commons-io/2.14.0", "e35d1df7232568ed4b81d9a9ed94a0159b3ff6cf1915cc21eef5cb55d7b157a2"),
}


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: fetch_replacement_jars.py NEW_OUTPUT_DIRECTORY")
    root = Path(sys.argv[1]).resolve()
    tmp = (Path.cwd() / ".tmp").resolve(strict=True)
    if not root.is_relative_to(tmp) or root == tmp:
        raise ValueError("output must be a new directory beneath .tmp")
    root.mkdir(parents=True, exist_ok=False)

    def fetch(name):
        path, expected = ITEMS[name]
        url = f"https://repo.maven.apache.org/maven2/{path}/{name}"
        with urllib.request.urlopen(url, timeout=90) as response:
            data = response.read(32 * 1024 * 1024 + 1)
        if len(data) > 32 * 1024 * 1024 or hashlib.sha256(data).hexdigest() != expected:
            raise ValueError(f"replacement digest mismatch: {name}")
        (root / name).write_bytes(data)
        return name

    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        items = list(pool.map(fetch, ITEMS))
    print(f"Verified {len(items)} replacement JARs")


if __name__ == "__main__":
    main()
