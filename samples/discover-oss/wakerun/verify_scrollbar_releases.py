#!/usr/bin/env python3
"""Check the exact published react-remove-scroll-bar 2.3.7/2.3.8 archives.

The inputs are read without extraction. This establishes release-file equality,
not that either archive was reproducibly built from the upstream Git tree.
"""

import argparse
import base64
import hashlib
import io
import json
from pathlib import Path
import tarfile


INTEGRITIES = {
    "2.3.7": "UzA0npskGFA2bUqwWsSRJscuQEt7zyfVtMpCtBl/lk+k/ky6lzYImEks9jwgxTDe/B0tb5hyrYgbQv/JQaHjDw==",
    "2.3.8": "9r+yi9+mgU33AKcj6IbT9oRCO78WriSj6t/cF8DWBZJ9aOGPOTEDvdUDz1FwKim7QXWwmHqtdHnRJfhAxEG46Q==",
}
LICENSE_SHA256 = "a79aae0c0f21990d9d963bb3c5a79cdcea9a46f8523ba55c58d7fe776b6ebc84"


def read_archive(path: Path, version: str) -> dict[str, bytes]:
    raw = path.read_bytes()
    digest = base64.b64encode(hashlib.sha512(raw).digest()).decode("ascii")
    if digest != INTEGRITIES[version]:
        raise ValueError(f"Unexpected {version} archive integrity")
    files: dict[str, bytes] = {}
    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:gz") as archive:
        for member in archive:
            if (
                not member.isfile()
                or not member.name.startswith("package/")
                or member.name in files
                or any(part in ("", ".", "..") for part in member.name.split("/"))
            ):
                raise ValueError("Unexpected archive entry")
            stream = archive.extractfile(member)
            if stream is None:
                raise ValueError("Archive file has no contents")
            files[member.name] = stream.read()
    return files


def verify(old_path: Path, new_path: Path, license_path: Path) -> int:
    old = read_archive(old_path, "2.3.7")
    new = read_archive(new_path, "2.3.8")
    if old.keys() != new.keys() or "package/package.json" not in old:
        raise ValueError("Published file sets differ")
    old_manifest = json.loads(old.pop("package/package.json"))
    new_manifest = json.loads(new.pop("package/package.json"))
    if any(old[name] != new[name] for name in old):
        raise ValueError("Published files differ outside the package manifest")
    if (
        old_manifest.get("name") != "react-remove-scroll-bar"
        or new_manifest.get("name") != "react-remove-scroll-bar"
        or old_manifest.get("version") != "2.3.7"
        or new_manifest.get("version") != "2.3.8"
        or old_manifest.get("license") != "MIT"
        or new_manifest.get("license") != "MIT"
    ):
        raise ValueError("Unexpected package identity")
    old_manifest["version"] = new_manifest["version"]
    old_dependencies = old_manifest.get("dependencies", {})
    new_dependencies = new_manifest.get("dependencies", {})
    if (
        old_dependencies.get("react-style-singleton") != "^2.2.1"
        or new_dependencies.get("react-style-singleton") != "^2.2.2"
    ):
        raise ValueError("Unexpected dependency range")
    old_dependencies["react-style-singleton"] = new_dependencies[
        "react-style-singleton"
    ]
    if old_manifest != new_manifest:
        raise ValueError("Unexpected additional package manifest change")
    if hashlib.sha256(license_path.read_bytes()).hexdigest() != LICENSE_SHA256:
        raise ValueError("Fallback license does not match the recorded upstream file")
    return len(old)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive_2_3_7", type=Path)
    parser.add_argument("archive_2_3_8", type=Path)
    parser.add_argument("fallback_license", type=Path)
    args = parser.parse_args()
    count = verify(args.archive_2_3_7, args.archive_2_3_8, args.fallback_license)
    print(f"Verified {count} identical non-manifest files and the pinned upstream license")
