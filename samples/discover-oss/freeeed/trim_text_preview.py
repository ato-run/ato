#!/usr/bin/env python3
"""Remove unused features from a local FreeEed *text-only preview* JAR.

This intentionally disables upstream capabilities and is not a full FreeEed
distribution. It only supports the narrowly tested preview in this directory.
"""

import argparse
import hashlib
from pathlib import Path
import re
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
SQLITE_3_41_2_2_SHA256 = "0cdab410947e04b6743df99cf1543267ddd107357d6f76948d145be590fd497d"
GUAVA_33_8_SHA256 = "97b79ec3f2c0a6313a0c2df51ce1abdf8907e6531803467b81efcaaf99b35161"
FAILUREACCESS_1_0_3_SHA256 = "cbfc3906b19b8f55dd7cfd6dfe0aa4532e834250d7f080bd8d211a3e246b59cb"
COMMONS_IO_2_14_SHA256 = "e35d1df7232568ed4b81d9a9ed94a0159b3ff6cf1915cc21eef5cb55d7b157a2"
SINGLE_TEXT_REMOVED_PREFIXES = (
    "com/lowagie/", "org/bouncycastle/", "com/sun/mail/", "javax/mail/",
    "javax/activation/", "com/sun/activation/", "com/sun/xml/", "javax/xml/bind/",
    "org/junit/", "junit/", "org/hamcrest/", "org/jvnet/hk2/", "com/sun/istack/",
    "javax/json/", "jakarta/ws/rs/", "jakarta/annotation/", "javax/xml/",
    "javax/servlet/", "org/aopalliance/", "com/sun/research/", "javassist/", "jersey/repackaged/", "nu/xom/", "mozilla/",
    "META-INF/maven/", "META-INF/native-image/", "META-INF/proguard/",
    "META-INF/versions/9/javax/xml/bind/", "META-INF/versions/21/org/glassfish/jersey/",
    "org/w3c/", "org/xml/sax/", "org/apache/xmlcommons/", "license/",
    "org/apache/commons/text/", "org/codehaus/jackson/", "com/jamesmurty/utils/",
    "com/fasterxml/jackson/", "com/google/gson/", "org/jsoup/", "org/apache/logging/log4j/",
    "org/apache/xerces/", "org/apache/xalan/",
    "org/apache/commons/beanutils/", "org/apache/commons/lang/ClassUtils",
    "org/apache/commons/httpclient/", "net/iharder/base64/", "java_cup/", "org/apache/html/",
    "org/apache/wml/", "org/apache/xml/serialize/", "org/apache/bcel/", "org/apache/xml/dtm/",
    "org/apache/xml/utils/", "org/apache/xpath/",
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
    parser.add_argument("--sqlite-jdbc", type=Path, help="replace sqlite-jdbc with pinned 3.41.2.2")
    parser.add_argument("--guava", type=Path, help="replace Guava with pinned 33.8.0-jre")
    parser.add_argument("--failureaccess", type=Path, help="the pinned Guava failureaccess runtime dependency")
    parser.add_argument("--commons-io", type=Path, help="replace Commons IO with pinned 2.14.0")
    args = parser.parse_args()
    if args.sqlite_jdbc and not args.single_text_extractor:
        parser.error("--sqlite-jdbc requires --single-text-extractor")
    if bool(args.guava) != bool(args.failureaccess) or (args.guava and not args.single_text_extractor):
        parser.error("--guava and --failureaccess must be supplied together with --single-text-extractor")
    if args.commons_io and not args.single_text_extractor:
        parser.error("--commons-io requires --single-text-extractor")
    engine, lucene = args.engine.resolve(strict=True), args.lucene_core.resolve(strict=True)
    output = args.output.resolve()
    tmp = (Path.cwd() / ".tmp").resolve(strict=True)
    if not output.is_relative_to(tmp) or output == tmp:
        parser.error("output must be a new file beneath .tmp")
    if sha256(engine) != args.engine_sha256 or sha256(lucene) != args.lucene_sha256:
        raise ValueError("input JAR digest mismatch")
    sqlite = args.sqlite_jdbc.resolve(strict=True) if args.sqlite_jdbc else None
    if sqlite and sha256(sqlite) != SQLITE_3_41_2_2_SHA256:
        raise ValueError("sqlite-jdbc digest mismatch")
    guava = args.guava.resolve(strict=True) if args.guava else None
    failureaccess = args.failureaccess.resolve(strict=True) if args.failureaccess else None
    if guava and (sha256(guava) != GUAVA_33_8_SHA256 or sha256(failureaccess) != FAILUREACCESS_1_0_3_SHA256):
        raise ValueError("Guava dependency digest mismatch")
    commons_io = args.commons_io.resolve(strict=True) if args.commons_io else None
    if commons_io and sha256(commons_io) != COMMONS_IO_2_14_SHA256:
        raise ValueError("Commons IO digest mismatch")
    with ZipFile(lucene) as core:
        core_names = set(core.namelist())
    prefixes = REMOVED_PREFIXES + (SINGLE_TEXT_REMOVED_PREFIXES if args.single_text_extractor else ())
    with ZipFile(engine) as original:
        names = original.namelist()
        missing = [prefix for prefix in prefixes if not any(name.startswith(prefix) for name in names)]
    if missing:
        raise ValueError(f"unexpected input layout; missing prefixes: {missing}")
    kept = 0
    services = []
    with ZipFile(engine) as source, ZipFile(output, "x", compression=ZIP_DEFLATED, allowZip64=True) as target:
        for entry in source.infolist():
            name = entry.filename
            if args.single_text_extractor and name in SINGLE_TEXT_REMOVED_FILES:
                continue
            versioned = re.fullmatch(r"META-INF/versions/[0-9]+/(.+)", name)
            if (args.single_text_extractor and versioned and
                    any(versioned.group(1).startswith(prefix) for prefix in SINGLE_TEXT_REMOVED_PREFIXES)):
                continue
            if sqlite and (name.startswith("org/sqlite/") or name == "sqlite-jdbc.properties"):
                continue
            if guava and name.startswith(("com/google/common/", "com/google/thirdparty/")):
                continue
            if commons_io and name.startswith("org/apache/commons/io/"):
                continue
            removed = next((prefix for prefix in prefixes if name.startswith(prefix)), None)
            if removed is not None:
                continue
            if name.startswith("org/apache/lucene/") and name not in core_names:
                continue
            if name.startswith("org/sqlite/native/") and not name.startswith(SQLITE_NATIVES):
                continue
            if entry.is_dir():
                continue
            if args.single_text_extractor and name.startswith("META-INF/services/"):
                services.append((entry, source.read(entry)))
                continue
            target.writestr(entry, source.read(entry))
            kept += 1
        if sqlite:
            with ZipFile(sqlite) as replacement:
                for entry in replacement.infolist():
                    name = entry.filename
                    if entry.is_dir() or not (name.startswith("org/sqlite/") or name == "sqlite-jdbc.properties"):
                        continue
                    if name.startswith("org/sqlite/native/") and not name.startswith(SQLITE_NATIVES):
                        continue
                    target.writestr(entry, replacement.read(entry))
                    kept += 1
        if guava:
            names = set()
            for replacement_path in (guava, failureaccess):
                with ZipFile(replacement_path) as replacement:
                    for entry in replacement.infolist():
                        name = entry.filename
                        if entry.is_dir() or not name.startswith(("com/google/common/", "com/google/thirdparty/")):
                            continue
                        if name in names:
                            raise ValueError("duplicate Guava dependency entry")
                        names.add(name)
                        target.writestr(entry, replacement.read(entry))
                        kept += 1
        if commons_io:
            with ZipFile(commons_io) as replacement:
                for entry in replacement.infolist():
                    if not entry.is_dir() and entry.filename.startswith("org/apache/commons/io/"):
                        target.writestr(entry, replacement.read(entry))
                        kept += 1
        # The shaded upstream JAR merges registrations for components removed
        # above. Keep only providers present in this self-contained preview;
        # dangling registrations otherwise break Java's ServiceLoader (and javac).
        if args.single_text_extractor:
            available = set(target.namelist())
            for entry, contents in services:
                providers = []
                for line in contents.decode("utf-8").splitlines():
                    provider = line.split("#", 1)[0].strip()
                    if provider and provider.replace(".", "/") + ".class" in available:
                        providers.append(provider)
                if providers:
                    target.writestr(entry, ("\n".join(providers) + "\n").encode("utf-8"))
                    kept += 1
    if kept == 0:
        output.unlink()
        raise ValueError("unexpected input layout; do not use the incomplete output")
    print(f"Text-only preview JAR: {output.stat().st_size} bytes; SHA-256 {sha256(output)}")


if __name__ == "__main__":
    main()
