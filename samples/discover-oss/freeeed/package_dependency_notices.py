#!/usr/bin/env python3
"""Package pinned dependency notices and verified source locations.

This assembles evidence; it does not independently determine compliance.
"""

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
from zipfile import ZIP_DEFLATED, ZipFile


EXTRAS = {
    "apache-2.0.txt": "bd48b462e828da5e34c975d725a829328cff0331dd4bf33abeb5619bb3d90ad2",
    "protobuf.txt": "6e5e117324afd944dcf67f36cf329843bc1a92229a8cd9bb573d7a83130fea7d",
    "simplemagic.txt": "9bf05a2b180c0bced886b90c6864bc6b5b75725b6068aab573bb26ed385a867d",
    "antlr.txt": "3db1fb3ee79a4b4f9918fc4d0f6133bf18a3cf787f126cd22f8aa9b862281c0c",
    "jsoup.txt": "ab973a9f42aca19cfba058ec6df5ce491cf35b9d77edf39c61effb8630b4b63d",
    "semver4j.txt": "46b963fd1a15de96ed9f022e1b5971e31d9bb544c32bb505f91d4721af2ae5a8",
    "reactive.txt": "69fa93e08eca4aef819fa0e9b35328d0e47ded0de3d57a9677db063fda64b5ab",
    "json.txt": "86cc84f1773e3318ba26fd61b3ae508a167df38b361695e105f6e499e7a2736f",
    "asm.html": "5feb76d8adda38acdeeba4e4301d0d8633bb0f40e1e802948b13cee21e68610e",
    "cc0.txt": "a2010f343487d3f7618affe54f789f5487602331c0a8d03f49e9a7c547cf0499",
}
EXTRA_URLS = {
    "apache-2.0.txt": "https://raw.githubusercontent.com/shmsoft/FreeEed/d268fac3c59beb91b3d9d77d7c386657d149ac4d/LICENSE",
    "protobuf.txt": "https://raw.githubusercontent.com/protocolbuffers/protobuf/d0bfd5221182da1a7cc280f3337b5e41a89539cf/LICENSE",
    "simplemagic.txt": "https://raw.githubusercontent.com/j256/simplemagic/3cdd981d41c19888908da626e8d9a9a130c6a2bf/LICENSE.txt",
    "antlr.txt": "https://raw.githubusercontent.com/antlr/antlr4/9239e6ff444420516b44b7621e8dc7691fcf0e16/LICENSE.txt",
    "jsoup.txt": "https://raw.githubusercontent.com/jhy/jsoup/5dc4c487d42705f60ff6e5b95d03c71a3326d6cd/LICENSE",
    "semver4j.txt": "https://raw.githubusercontent.com/semver4j/semver4j/d0070c4fb45baa7da3b74c6a3cd73e49ca18dd08/LICENSE",
    "reactive.txt": "https://raw.githubusercontent.com/reactive-streams/reactive-streams-jvm/7207b1b4fb618c0cf17f3391a5e551648c75769d/LICENSE",
    "json.txt": "https://raw.githubusercontent.com/stleary/JSON-java/f346203cd663bb680cad0d5894e7c147e36f31cd/LICENSE",
    "asm.html": "https://asm.ow2.io/license.html",
    "cc0.txt": "https://raw.githubusercontent.com/spdx/license-list-data/main/text/CC0-1.0.txt",
}
SUPPLEMENT = {
    "com.google.protobuf:protobuf-java:3.11.4": ["protobuf.txt"],
    "com.j256.simplemagic:simplemagic:1.17": ["simplemagic.txt"],
    "org.antlr:antlr4-runtime:4.11.1": ["antlr.txt"],
    "org.json:json:20231013": ["json.txt"],
    "org.jsoup:jsoup:1.9.2": ["jsoup.txt"],
    "org.reactivestreams:reactive-streams:1.0.3": ["reactive.txt", "cc0.txt"],
    "org.semver4j:semver4j:5.8.0": ["semver4j.txt"],
    **{f"org.ow2.asm:{name}:7.2": ["asm.html"] for name in ("asm", "asm-analysis", "asm-commons", "asm-tree")},
}
MISSING_POM_LICENSE = {
    "commons-httpclient:commons-httpclient:3.1",
    "org.codehaus.jackson:jackson-core-asl:1.9.13",
    "org.codehaus.jackson:jackson-mapper-asl:1.9.13",
}


def sha(data):
    return hashlib.sha256(data).hexdigest()


def notice_path(name):
    parts = PurePosixPath(name).parts
    if not parts or ".." in parts:
        raise ValueError("unsafe notice path")
    leaf = parts[-1].lower()
    return bool(re.match(r"^(license|licence|copying|copyright|notice|third.party|asl2\.0)", leaf)) or (
        "license" in (part.lower() for part in parts[:-1]) and leaf.startswith("readme")
    )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("inventory", type=Path)
    parser.add_argument("provenance", type=Path)
    parser.add_argument("maven", type=Path)
    parser.add_argument("sources", type=Path)
    parser.add_argument("extras", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    inventory = json.loads(args.inventory.read_text())
    provenance = json.loads(args.provenance.read_text())
    if inventory.get("kind") != "preliminary-trimmed-entry-inventory/1" or provenance.get("kind") != "trimmed-byte-provenance/1":
        raise ValueError("unexpected input inventory")
    if provenance["unmatched"] or inventory["preview_sha256"] != provenance["preview_sha256"]:
        raise ValueError("unmatched or inconsistent preview provenance")
    dependencies = inventory["dependencies"]
    by_coordinate = {entry["coordinate"]: entry for entry in dependencies}
    if set(provenance["dependency_coordinates"]) - set(by_coordinate):
        raise ValueError("a retained class lacks a dependency notice entry")
    if {d["coordinate"] for d in dependencies if not d["pom_licenses"]} - MISSING_POM_LICENSE:
        raise ValueError("unexpected dependencies without POM license metadata")
    source_index = {entry["coordinate"]: entry for entry in json.loads((args.sources / "inventory.json").read_text())}
    tmp = (Path.cwd() / ".tmp").resolve(strict=True)
    if not args.output.resolve().is_relative_to(tmp):
        raise ValueError("output must be under .tmp")
    manifest = []
    with ZipFile(args.output, "x", compression=ZIP_DEFLATED) as out:
        for name, expected in EXTRAS.items():
            data = (args.extras / name).read_bytes()
            if sha(data) != expected:
                raise ValueError(f"supplemental license digest mismatch: {name}")
            out.writestr("supplemental/" + name, data)
        for item in dependencies:
            coord = item["coordinate"]
            group, artifact, version = coord.split(":")
            classifier = item.get("classifier")
            suffix = f"-{classifier}" if classifier else ""
            binary = args.maven.joinpath(*group.split("."), artifact, version, f"{artifact}-{version}{suffix}.jar")
            if sha(binary.read_bytes()) != item["sha256"]:
                raise ValueError(f"binary digest mismatch: {coord}")
            source = source_index.get(coord)
            if not source or source["status"] != "available":
                raise ValueError(f"missing exact source artifact: {coord}")
            source_file = args.sources.joinpath(*group.split("."), artifact, version, f"{artifact}-{version}-sources.jar")
            if sha(source_file.read_bytes()) != source["source_sha256"]:
                raise ValueError(f"source digest mismatch: {coord}")
            notices = []
            binary_evidence = bytearray()
            for label, archive_path in (("binary", binary), ("source", source_file)):
                with ZipFile(archive_path) as jar:
                    if label == "source" and not any(n.lower().endswith((".java", ".kt", ".scala", ".groovy", ".c", ".h", ".py")) for n in jar.namelist()):
                        raise ValueError(f"source archive contains no code: {coord}")
                    for entry in jar.infolist():
                        if entry.is_dir() or not notice_path(entry.filename):
                            continue
                        dest = f"dependencies/{group}/{artifact}/{version}/{label}/{entry.filename}"
                        data = jar.read(entry)
                        out.writestr(dest, data)
                        if label == "binary":
                            binary_evidence.extend(data)
                        notices.append(dest)
            if coord in MISSING_POM_LICENSE and (b"Apache" not in binary_evidence or b"2.0" not in binary_evidence):
                raise ValueError(f"embedded Apache license evidence missing: {coord}")
            licenses = item["pom_licenses"] or [{"name": "Apache License, Version 2.0", "url": "https://www.apache.org/licenses/LICENSE-2.0", "evidence": "embedded binary license and notice"}]
            descriptions = " ".join(x["name"] for x in licenses).lower()
            if any(name in descriptions for name in ("gpl", "lesser", "cddl", "mozilla", "mpl", "eclipse", "epl")):
                raise ValueError(f"copyleft dependency requires separate review: {coord}")
            supplemental = SUPPLEMENT.get(coord, [])
            if "apache" not in descriptions and not notices and not supplemental:
                raise ValueError(f"non-Apache license text missing: {coord}")
            manifest.append({"coordinate": coord, "binary_sha256": item["sha256"], "licenses": licenses,
                             "source_url": source["url"], "source_sha256": source["source_sha256"],
                             "embedded_notices": notices, "supplemental_notices": supplemental})
        out.writestr("dependency-manifest.json", json.dumps(manifest, indent=2) + "\n")
        out.writestr("provenance.json", json.dumps(provenance, indent=2) + "\n")
        out.writestr("supplemental-sources.json", json.dumps({name: {"url": EXTRA_URLS[name], "sha256": digest} for name, digest in EXTRAS.items()}, indent=2) + "\n")
        out.writestr("README.txt", "Pinned dependency notices and upstream source locations for the synthetic text-only FreeEed preview. Source archives are linked by URL and SHA-256 in dependency-manifest.json. The byte provenance inventory is evidence, not a guarantee of complete legal review.\n")
    print(f"Packaged notices for {len(manifest)} dependency candidates: {args.output.stat().st_size} bytes")


if __name__ == "__main__":
    main()
