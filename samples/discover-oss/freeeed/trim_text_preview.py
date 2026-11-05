#!/usr/bin/env python3
"""Remove unused features from a local FreeEed *text-only preview* JAR.

This intentionally disables upstream capabilities and is not a full FreeEed
distribution. It only supports the narrowly tested preview in this directory.
"""

import argparse
import hashlib
from pathlib import Path
from zipfile import ZIP_DEFLATED, ZipFile


REMOVED_PREFIXES = (
    "org/apache/hadoop/", "org/apache/solr/", "edu/stanford/nlp/",
    "software/amazon/awssdk/", "com/apple/", "org/apache/poi/",
    "org/apache/pdfbox/", "org/apache/fontbox/", "org/xerial/snappy/",
    "org/eclipse/jetty/", "org/glassfish/", "io/netty/",
    "org/apache/zookeeper/", "org/apache/htrace/", "com/carrotsearch/",
    "com/google/protobuf/", "META-INF/native/", "text/",
)
SQLITE_NATIVES = ("org/sqlite/native/Linux/x86_64/", "org/sqlite/native/Mac/aarch64/")


def sha256(path: Path) -> str:
    with path.open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("engine", type=Path)
    parser.add_argument("lucene_core", type=Path)
    parser.add_argument("output", type=Path, help="new file beneath .tmp")
    parser.add_argument("--engine-sha256", required=True)
    parser.add_argument("--lucene-sha256", required=True)
    args = parser.parse_args()
    engine, lucene = args.engine.resolve(strict=True), args.lucene_core.resolve(strict=True)
    output = args.output.resolve()
    tmp = (Path.cwd() / ".tmp").resolve(strict=True)
    if not output.is_relative_to(tmp) or output == tmp:
        parser.error("output must be a new file beneath .tmp")
    if sha256(engine) != args.engine_sha256 or sha256(lucene) != args.lucene_sha256:
        raise ValueError("input JAR digest mismatch")
    with ZipFile(lucene) as core:
        core_names = set(core.namelist())
    counts = {prefix: 0 for prefix in REMOVED_PREFIXES}
    kept = 0
    with ZipFile(engine) as source, ZipFile(output, "x", compression=ZIP_DEFLATED, allowZip64=True) as target:
        for entry in source.infolist():
            name = entry.filename
            removed = next((prefix for prefix in REMOVED_PREFIXES if name.startswith(prefix)), None)
            if removed is not None:
                counts[removed] += 1
                continue
            if name.startswith("org/apache/lucene/") and name not in core_names:
                continue
            if name.startswith("org/sqlite/native/") and not name.startswith(SQLITE_NATIVES):
                continue
            if entry.is_dir():
                continue
            target.writestr(entry, source.read(entry))
            kept += 1
    if any(count == 0 for count in counts.values()) or kept == 0:
        raise ValueError("unexpected input layout; do not use the incomplete output")
    print(f"Text-only preview JAR: {output.stat().st_size} bytes; SHA-256 {sha256(output)}")


if __name__ == "__main__":
    main()
