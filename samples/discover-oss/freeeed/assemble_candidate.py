#!/usr/bin/env python3
"""Assemble a local, unverified FreeEed service-group candidate under .tmp."""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
from zipfile import ZIP_DEFLATED, ZipFile


UPSTREAM = "d268fac3c59beb91b3d9d77d7c386657d149ac4d"
CHUNK_BYTES = 60 * 1024 * 1024


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("upstream", type=Path, help="pinned upstream Git checkout")
    parser.add_argument("build", type=Path, help="patched Maven source/build tree")
    parser.add_argument("output", type=Path, help="new directory beneath .tmp")
    parser.add_argument("--engine", type=Path, help="explicit text-only preview JAR")
    parser.add_argument("--single-text", action="store_true", help="use the local text extractor and a single OCI service")
    parser.add_argument("--notices", type=Path, help="dependency notices ZIP for this exact engine")
    args = parser.parse_args()
    if args.single_text and not args.engine:
        parser.error("--single-text requires an explicitly trimmed --engine")
    if args.notices and not args.single_text:
        parser.error("--notices is supported for the single-text candidate only")
    upstream = args.upstream.resolve(strict=True)
    build = args.build.resolve(strict=True)
    output = args.output.resolve()
    tmp = (Path.cwd() / ".tmp").resolve(strict=True)
    if not output.is_relative_to(tmp) or output == tmp:
        parser.error("output must be a new directory beneath .tmp")
    revision = subprocess.check_output(["git", "-C", str(upstream), "rev-parse", "HEAD"], text=True).strip()
    if revision != UPSTREAM:
        raise ValueError("unexpected upstream revision")
    artifacts = build / "freeeed-processing/target"
    jar = (args.engine.resolve(strict=True) if args.engine else
           artifacts / "freeeed-processing-1.0-SNAPSHOT-jar-with-dependencies.jar")
    sources = artifacts / "freeeed-processing-1.0-SNAPSHOT-sources.jar"
    if not jar.is_file() or not sources.is_file():
        raise FileNotFoundError("build the patched engine and source JAR first")
    local = Path(__file__).resolve().parent
    output.mkdir(parents=True, exist_ok=False)
    chunks = output / "engine"
    chunks.mkdir()
    digest = hashlib.sha256()
    with jar.open("rb") as src:
        index = 0
        while chunk := src.read(CHUNK_BYTES):
            digest.update(chunk)
            (chunks / f"part-{index:02d}").write_bytes(chunk)
            index += 1
    if not index or index > 99:
        raise ValueError("unexpected engine size")
    (output / "engine.sha256").write_text(digest.hexdigest() + "\n")
    manifest = "single-text.capsule.toml" if args.single_text else "capsule.template.toml"
    startup = "start-single-text.sh" if args.single_text else "start.sh"
    shutil.copyfile(local / manifest, output / "capsule.toml")
    shutil.copyfile(local / startup, output / startup)
    preview = output / "preview"
    preview.mkdir()
    subprocess.run(["javac", "--release", "17", "-d", str(preview),
                    str(local / "ProcessingPreview.java"), str(local / "TextOnlyExtractor.java")], check=True)
    source = output / "source"
    source.mkdir()
    if args.notices:
        notices = args.notices.resolve(strict=True)
        with ZipFile(notices) as notice_archive:
            provenance = json.loads(notice_archive.read("provenance.json"))
            if (provenance.get("kind") != "trimmed-byte-provenance/1" or provenance.get("unmatched")
                    or provenance.get("preview_sha256") != digest.hexdigest()):
                raise ValueError("notices do not identify the exact trimmed engine")
            if not json.loads(notice_archive.read("dependency-manifest.json")):
                raise ValueError("empty dependency notice manifest")
        shutil.copyfile(notices, source / "third-party-notices.zip")
        audited = json.loads((local / "single-text-inventory.json").read_text())
        if audited.get("preview_sha256") != digest.hexdigest():
            raise ValueError("retained inventory does not identify the exact trimmed engine")
        for name in ("single-text-inventory.json", "single-text-provenance.json"):
            shutil.copyfile(local / name, source / name)
    shutil.copyfile(local.parents[2] / "LICENSE", source / "ATO-ADAPTER-LICENSE-MPL-2.0.txt")
    shutil.copyfile(sources, source / "freeeed-processing-sources.jar")
    for name in ("ProcessingPreview.java", "patch_upstream.py", "assemble_candidate.py",
                 "trim_text_preview.py", "make_dependency_inventory.py", "dependency-inventory.json",
                 "synthetic_check.py", "test_preview.py", "start.sh", "capsule.template.toml", "README.md",
                 "TextOnlyExtractor.java", "start-single-text.sh", "single-text.capsule.toml",
                 "audit_trimmed_provenance.py", "package_dependency_notices.py", "collect_dependency_sources.py",
                 "fetch_extra_licenses.py", "replace_sqlite_inventory.py", "fetch_replacement_jars.py"):
        shutil.copyfile(local / name, source / name)
    for filename in ("LICENSE", "freeeed-processing/NOTICE", "pom.xml", "freeeed-processing/pom.xml"):
        target = source / filename
        target.parent.mkdir(parents=True, exist_ok=True)
        with target.open("wb") as file:
            subprocess.run(["git", "-C", str(upstream), "show", f"{UPSTREAM}:{filename}"],
                           check=True, stdout=file)
    with ZipFile(output / "source.zip", "x", compression=ZIP_DEFLATED) as archive:
        for path in sorted(source.rglob("*")):
            if path.is_file():
                archive.write(path, path.relative_to(source).as_posix())
    print(f"Local candidate assembled at {output}; engine SHA-256 {digest.hexdigest()}")


if __name__ == "__main__":
    main()
