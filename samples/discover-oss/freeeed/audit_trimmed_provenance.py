#!/usr/bin/env python3
"""Match retained application files and classes to pinned input artifacts.

Byte matches prove inclusion evidence, not legal compliance or functional
equivalence. Maven service metadata may be merged and is reported separately.
"""

import argparse
import hashlib
import json
from pathlib import Path
from zipfile import ZipFile


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("preview", type=Path)
    parser.add_argument("application_jar", type=Path)
    parser.add_argument("inventory", type=Path)
    parser.add_argument("maven_repository", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--application-sha256", required=True)
    args = parser.parse_args()
    if digest(args.application_jar) != args.application_sha256:
        raise ValueError("application JAR digest mismatch")
    inventory = json.loads(args.inventory.read_text())
    if inventory.get("kind") != "preliminary-maven-runtime-inventory/1":
        raise ValueError("unexpected Maven inventory")
    with ZipFile(args.preview) as target:
        entries = {entry.filename: entry for entry in target.infolist() if not entry.is_dir() and
                   (not entry.filename.startswith("META-INF/") or entry.filename.endswith(".class"))}
        owners = {}

        def match(archive, owner):
            count = 0
            for entry in archive.infolist():
                candidate = entries.get(entry.filename)
                if (candidate is None or entry.CRC != candidate.CRC or entry.file_size != candidate.file_size):
                    continue
                if archive.read(entry) == target.read(candidate):
                    owners.setdefault(entry.filename, []).append(owner)
                    count += 1
            return count

        with ZipFile(args.application_jar) as app:
            match(app, "freeeed-processing")
        for item in inventory["dependencies"]:
            group, artifact, version = item["coordinate"].split(":")
            classifier = item.get("classifier")
            suffix = f"-{classifier}" if classifier else ""
            jar = args.maven_repository.joinpath(*group.split("."), artifact, version,
                                                  f"{artifact}-{version}{suffix}.jar")
            if digest(jar) != item["sha256"]:
                raise ValueError(f"dependency digest mismatch: {item['coordinate']}")
            with ZipFile(jar) as dependency:
                match(dependency, item["coordinate"])
        unknown = sorted(set(entries) - set(owners))
        deps = sorted({owner for values in owners.values() for owner in values if owner != "freeeed-processing"})
        result = {"kind": "trimmed-byte-provenance/1", "preview_sha256": digest(args.preview),
                  "application_sha256": args.application_sha256, "files_checked": len(entries),
                  "unmatched": unknown, "dependency_coordinates": deps,
                  "warning": "Byte matches are inclusion evidence, not a complete legal or runtime audit. META-INF non-class files can be merged."}
        args.output.write_text(json.dumps(result, indent=2) + "\n")
        print(f"checked {len(entries)} files, {len(unknown)} unmatched, {len(deps)} dependency candidates")
        if unknown:
            raise SystemExit("unmatched files require review")


if __name__ == "__main__":
    main()
