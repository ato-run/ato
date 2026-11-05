#!/usr/bin/env python3
"""Record Maven's resolved runtime dependencies and available POM license fields.

The output is a preliminary inventory, not a verified distribution SBOM or a
conclusion about redistribution rights. Maven POM license lists may describe
alternatives, bundled components, or incomplete metadata.
"""

import argparse
import hashlib
import json
from pathlib import Path
import xml.etree.ElementTree as ET


NAMESPACE = {"m": "http://maven.apache.org/POM/4.0.0"}
def pom_licenses(repo: Path, coordinate: tuple[str, str, str], seen=None):
    seen = seen or set()
    if coordinate in seen:
        return [], None
    seen.add(coordinate)
    group, artifact, version = coordinate
    pom = repo.joinpath(*group.split("."), artifact, version, f"{artifact}-{version}.pom")
    if not pom.is_file():
        return [], None
    root = ET.parse(pom).getroot()
    licenses = [
        {"name": (item.findtext("m:name", default="", namespaces=NAMESPACE)).strip(),
         "url": (item.findtext("m:url", default="", namespaces=NAMESPACE)).strip()}
        for item in root.findall("m:licenses/m:license", NAMESPACE)
    ]
    if licenses:
        return licenses, ":".join(coordinate)
    parent = root.find("m:parent", NAMESPACE)
    if parent is None:
        return [], None
    parts = tuple(parent.findtext(f"m:{key}", default="", namespaces=NAMESPACE)
                  for key in ("groupId", "artifactId", "version"))
    if not all(parts) or any("$" in part for part in parts):
        return [], None
    return pom_licenses(repo, parts, seen)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("dependency_tree", type=Path)
    parser.add_argument("maven_repository", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    lines = args.dependency_tree.read_text().splitlines()
    if not lines or lines[0] != "org.freeeed:freeeed-processing:jar:1.0-SNAPSHOT":
        parser.error("unexpected dependency tree root")
    coordinates = set()
    for line in lines[1:]:
        parts = line.lstrip(" |+-\\").split(":")
        if parts[-1] not in ("compile", "runtime"):
            continue
        if len(parts) not in (5, 6) or parts[2] not in ("jar", "bundle"):
            raise ValueError(f"unsupported dependency-tree line: {line}")
        group, artifact = parts[:2]
        classifier = parts[3] if len(parts) == 6 else ""
        version = parts[-2]
        coordinates.add((group, artifact, version, classifier))
    if not coordinates:
        raise ValueError("no compile or runtime dependencies")
    result = []
    for group, artifact, version, classifier in sorted(coordinates):
        suffix = f"-{classifier}" if classifier else ""
        jar = args.maven_repository.joinpath(*group.split("."), artifact, version,
                                               f"{artifact}-{version}{suffix}.jar")
        with jar.open("rb") as file:
            digest = hashlib.file_digest(file, "sha256").hexdigest()
        licenses, pom_source = pom_licenses(args.maven_repository, (group, artifact, version))
        result.append({"coordinate": f"{group}:{artifact}:{version}", "classifier": classifier or None,
                       "sha256": digest,
                       "pom_licenses": licenses, "license_pom": pom_source})
    output = {"kind": "preliminary-maven-runtime-inventory/1",
              "upstream_commit": "d268fac3c59beb91b3d9d77d7c386657d149ac4d",
              "warning": "POM metadata is not a verified distribution license or source inventory.",
              "dependencies": result}
    args.output.write_text(json.dumps(output, indent=2, ensure_ascii=False) + "\n")
    print(f"Recorded {len(result)} dependencies; "
          f"{sum(not item['pom_licenses'] for item in result)} lack POM license metadata")


if __name__ == "__main__":
    main()
