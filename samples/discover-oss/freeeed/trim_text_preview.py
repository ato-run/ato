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
SINGLE_TEXT_REMOVED_PREFIXES = (
    "com/lowagie/", "org/bouncycastle/", "com/sun/mail/", "javax/mail/",
    "javax/activation/", "com/sun/activation/", "com/sun/xml/", "javax/xml/bind/",
    "org/junit/", "junit/", "org/hamcrest/", "org/jvnet/hk2/", "com/sun/istack/",
    "javax/json/", "jakarta/ws/rs/", "jakarta/annotation/", "javax/xml/",
    "javax/servlet/", "org/aopalliance/", "com/sun/research/", "javassist/", "jersey/repackaged/", "nu/xom/", "mozilla/",
    "META-INF/maven/", "META-INF/native-image/", "META-INF/proguard/",
    "META-INF/versions/9/javax/xml/bind/", "META-INF/versions/21/org/glassfish/jersey/",
    "org/w3c/", "org/xml/sax/", "org/apache/xmlcommons/", "license/",
)
SINGLE_TEXT_REMOVED_FILES = frozenset({
    "LICENSE-junit.txt", "jetty-dir.css", "about.html", "krb5-template.conf", "krb5_udp-template.conf",
    "META-INF/versions/9/module-info.class", "META-INF/versions/11/module-info.class",
})


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
    parser.add_argument("--single-text-extractor", action="store_true",
                        help="also remove PDF, mail, crypto and other unsupported libraries")
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
    prefixes = REMOVED_PREFIXES + (SINGLE_TEXT_REMOVED_PREFIXES if args.single_text_extractor else ())
    counts = {prefix: 0 for prefix in prefixes}
    kept = 0
    with ZipFile(engine) as source, ZipFile(output, "x", compression=ZIP_DEFLATED, allowZip64=True) as target:
        for entry in source.infolist():
            name = entry.filename
            if args.single_text_extractor and name in SINGLE_TEXT_REMOVED_FILES:
                continue
            removed = next((prefix for prefix in prefixes if name.startswith(prefix)), None)
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
