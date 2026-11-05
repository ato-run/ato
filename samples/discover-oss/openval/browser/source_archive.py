#!/usr/bin/env python3
"""Build the source-and-notices download for the offline OpenVal preview."""

import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import sys
import tarfile
from zipfile import ZIP_DEFLATED, ZipFile


UPSTREAM_SHA256 = "cb66b3179df2ef6f5e4c85bfd4c5c8a22a22c017dc2f0532d0ada29c20e6e549"
ARCHIVE_HASHES = {
    "openpyxl.tar.gz": "cf0e3cf56142039133628b5acffe8ef0c12bc902d2aadd3e0fe5878dc08d1050",
    "et_xmlfile.tar.gz": "dab3f4764309081ce75662649be815c4c9081e88f0837825f90fd28317d4da54",
    "pyodide-LICENSE": "1f256ecad192880510e84ad60474eab7589218784b9a50bc7ceee34c2b91f1d5",
    "cpython-LICENSE": "78b12c3a81360b357002334f0e70ea0e92eebf7a9b358805c03c48484945f3bb",
}


def verify(path, expected):
    data = path.read_bytes()
    if hashlib.sha256(data).hexdigest() != expected:
        raise ValueError(f"digest mismatch: {path}")
    return data


def main():
    if len(sys.argv) != 6:
        raise SystemExit("usage: source_archive.py UPSTREAM_TAR BUILD_ROOT WHEELS LICENSES OUTPUT")
    upstream, build, wheels, licenses, output = map(Path, sys.argv[1:])
    overlay = Path(__file__).resolve().parent
    upstream_bytes = verify(upstream, UPSTREAM_SHA256)
    with ZipFile(output, "w", compression=ZIP_DEFLATED) as out:
        with tarfile.open(fileobj=io.BytesIO(upstream_bytes), mode="r:gz") as tar:
            for item in tar:
                if not item.isfile():
                    continue
                parts = PurePosixPath(item.name).parts
                if len(parts) < 2 or ".." in parts:
                    raise ValueError("unsafe upstream archive path")
                out.writestr("upstream/" + "/".join(parts[1:]), tar.extractfile(item).read())
        for file in sorted(overlay.iterdir()):
            if file.is_file():
                out.write(file, "adapter/" + file.name)
        for name in ("web/app/page.tsx", "web/lib/api.ts", "web/next.config.mjs"):
            out.write(build / name, "modified/" + name)
        with ZipFile(build / "web/public/openval-engine.zip") as engine:
            for name in engine.namelist():
                out.writestr("modified/engine/" + name, engine.read(name))
        for item in json.loads((overlay / "wheel-lock.json").read_text()):
            wheel = wheels / item["filename"]
            with ZipFile(io.BytesIO(verify(wheel, item["sha256"]))) as archive:
                for name in archive.namelist():
                    leaf = PurePosixPath(name).name.lower()
                    if any(token in leaf for token in ("license", "licence", "copying", "notice")):
                        out.writestr("notices/python/" + wheel.name + "/" + name, archive.read(name))
        for name in ("openpyxl.tar.gz", "et_xmlfile.tar.gz"):
            with tarfile.open(fileobj=io.BytesIO(verify(licenses / name, ARCHIVE_HASHES[name])), mode="r:gz") as tar:
                for item in tar:
                    if item.isfile() and "licence" in PurePosixPath(item.name).name.lower():
                        out.writestr("notices/python/" + name + "/" + PurePosixPath(item.name).name, tar.extractfile(item).read())
        for name in ("pyodide-LICENSE", "cpython-LICENSE"):
            out.writestr("notices/runtime/" + name, verify(licenses / name, ARCHIVE_HASHES[name]))
        modules = build / "web/node_modules"
        for file in modules.rglob("*"):
            if file.is_file() and file.name.lower().startswith(("license", "licence", "copying", "notice")):
                out.write(file, "notices/npm/" + file.relative_to(modules).as_posix())
        out.writestr("SOURCE-AND-NOTICES.txt", """OpenVal browser preview: source and third-party notices
OpenVal upstream (MIT), commit 89f471751e498813c8458cdc1f6bdc5a54b78e2a:
https://github.com/aanfinson00/openval/tree/89f471751e498813c8458cdc1f6bdc5a54b78e2a
The adapter and modified sources are included here. The modified engine fixes
round-tripping an empty optional gross-up value in exported workbooks.
Unmodified Pyodide 0.29.4 (MPL-2.0) source:
https://github.com/pyodide/pyodide/tree/0.29.4
Its CPython 3.13.2 source: https://github.com/python/cpython/tree/v3.13.2
Wheel URLs, hashes, and exact versions are in adapter/wheel-lock.json.
Node dependency versions are in adapter/package-lock.json. Dependency license
texts included in their distributions are under notices/. The software is a
pre-alpha preview and its financial calculations are not validated.
""")
    print(f"source archive: {output.stat().st_size} bytes")


if __name__ == "__main__":
    main()
