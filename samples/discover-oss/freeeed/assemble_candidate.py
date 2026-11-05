#!/usr/bin/env python3
"""Assemble a local, unverified FreeEed service-group candidate under .tmp."""

import argparse
import hashlib
from pathlib import Path
import shutil
import subprocess


UPSTREAM = "d268fac3c59beb91b3d9d77d7c386657d149ac4d"
CHUNK_BYTES = 60 * 1024 * 1024


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("upstream", type=Path, help="pinned upstream Git checkout")
    parser.add_argument("build", type=Path, help="patched Maven source/build tree")
    parser.add_argument("output", type=Path, help="new directory beneath .tmp")
    args = parser.parse_args()
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
    jar = artifacts / "freeeed-processing-1.0-SNAPSHOT-jar-with-dependencies.jar"
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
    shutil.copyfile(local / "capsule.template.toml", output / "capsule.toml")
    shutil.copyfile(local / "start.sh", output / "start.sh")
    preview = output / "preview"
    preview.mkdir()
    subprocess.run(["javac", "--release", "17", "-d", str(preview),
                    str(local / "ProcessingPreview.java")], check=True)
    source = output / "source"
    source.mkdir()
    shutil.copyfile(sources, source / "freeeed-processing-sources.jar")
    for name in ("ProcessingPreview.java", "patch_upstream.py", "assemble_candidate.py",
                 "start.sh", "capsule.template.toml", "README.md"):
        shutil.copyfile(local / name, source / name)
    for filename in ("LICENSE", "freeeed-processing/NOTICE", "pom.xml", "freeeed-processing/pom.xml"):
        target = source / filename
        target.parent.mkdir(parents=True, exist_ok=True)
        with target.open("wb") as file:
            subprocess.run(["git", "-C", str(upstream), "show", f"{UPSTREAM}:{filename}"],
                           check=True, stdout=file)
    print(f"Local candidate assembled at {output}; engine SHA-256 {digest.hexdigest()}")


if __name__ == "__main__":
    main()
