#!/usr/bin/env python3
"""Find dependency JAR entries byte-identical to files in a trimmed preview."""

import argparse
import hashlib
import json
from pathlib import Path
from zipfile import ZipFile


def sha256(path: Path) -> str:
    with path.open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("preview", type=Path)
    parser.add_argument("inventory", type=Path)
    parser.add_argument("maven_repository", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    inventory = json.loads(args.inventory.read_text())
    if inventory.get("kind") != "preliminary-maven-runtime-inventory/1":
        raise ValueError("unexpected inventory")
    retained = []
    with ZipFile(args.preview) as target:
        entries = {entry.filename: entry for entry in target.infolist()
                   if not entry.is_dir() and not entry.filename.startswith("META-INF/")}
        for item in inventory["dependencies"]:
            group, artifact, version = item["coordinate"].split(":")
            classifier = item.get("classifier")
            suffix = f"-{classifier}" if classifier else ""
            jar = args.maven_repository.joinpath(*group.split("."), artifact, version,
                                                  f"{artifact}-{version}{suffix}.jar")
            if sha256(jar) != item["sha256"]:
                raise ValueError(f"dependency JAR digest mismatch: {item['coordinate']}")
            matched = 0
            with ZipFile(jar) as dependency:
                for entry in dependency.infolist():
                    candidate = entries.get(entry.filename)
                    if (candidate is None or entry.CRC != candidate.CRC
                            or entry.file_size != candidate.file_size):
                        continue
                    if hashlib.sha256(dependency.read(entry)).digest() == hashlib.sha256(target.read(candidate)).digest():
                        matched += 1
            if matched:
                retained.append({**item, "byte_identical_entries": matched})
    output = {
        "kind": "preliminary-trimmed-entry-inventory/1",
        "preview_sha256": sha256(args.preview),
        "warning": "Byte matches are evidence of inclusion, not a complete SBOM or license review.",
        "dependencies": retained,
    }
    args.output.write_text(json.dumps(output, indent=2, ensure_ascii=False) + "\n")
    print(f"{len(retained)} dependencies have matching entries; "
          f"{sum(not item['pom_licenses'] for item in retained)} lack POM license metadata")


if __name__ == "__main__":
    main()
