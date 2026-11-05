#!/usr/bin/env python3
"""Exercise a locally built FreeEed engine against an isolated Tika server.

This is an exploratory test, not an application adapter or a deployment gate.
On macOS it confines writes to a fresh test directory and restricts outgoing
connections to the caller's local Tika server on port 9998.
"""

import argparse
import csv
import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import zipfile


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("jar", type=Path, help="built FreeEed processing JAR")
    parser.add_argument("test_root", type=Path, help="existing private test root")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("this test currently requires macOS sandbox-exec")
    jar = args.jar.resolve(strict=True)
    root = args.test_root.resolve(strict=True)
    if not root.is_dir() or not jar.is_file():
        parser.error("the test root must be a directory and the JAR a file")
    run = Path(tempfile.mkdtemp(prefix="freeeed-fixture-", dir=root))
    # sandbox-exec's profile is a string; do not interpolate unsafe paths.
    if any(c in str(run) for c in ('"', "\\", "\n", "\r")):
        raise ValueError("unsupported test-root path")

    source = b"Synthetic test document for FreeEed.\nReference: staging fixture only.\n"
    (run / "input").mkdir()
    (run / "input" / "fixture.txt").write_bytes(source)
    settings = {
        "project-name": "Synthetic staging fixture",
        "project-code": "fixture",
        "input": str(run / "input"),
        "dir-active": "y",
        "custodian": "fixture",
        "data_source": "0",
        "stage": "true",
        "stage-in-place": "false",
        "process-where": "local",
        "file-system": "local",
        "files-per-zip-staging": "50",
        "gigs-per-zip-staging": "0.1",
        "field-separator": "pipe",
        "metadata-file-ext": "CSV",
        "metadata": "standard",
        "load_file_format": "CSV",
        "create-pdf": "false",
        "preview": "false",
        "lucene_fs_index_enabled": "false",
        "send_index_solr_enabled": "false",
        "ocr_enabled": "false",
        "pii_active": "false",
        "summarize_active": "false",
        "transcribe": "false",
    }
    project = run / "fixture.project"
    project.write_text("".join(f"{key}={value}\n" for key, value in settings.items()))
    profile = (
        '(version 1)(allow default)'
        '(deny network-outbound (require-not (remote ip "localhost:9998")))'
        '(deny file-write*)'
        f'(allow file-write* (subpath "{run}") (literal "/dev/null"))'
    )
    result = subprocess.run(
        ["sandbox-exec", "-p", profile, "java", f"-Djava.io.tmpdir={run}",
         f"-Duser.home={run}", "-Djava.awt.headless=true",
         "-Djava.net.preferIPv4Stack=true", "-jar", str(jar),
         "-param_file", str(project)],
        cwd=run,
        env={**os.environ, "TMPDIR": str(run)},
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=120,
        check=True,
    )
    (run / "engine.log").write_bytes(result.stdout)
    if b"Failed to extract metadata" in result.stdout:
        raise RuntimeError(f"FreeEed reported extraction failure; see {run / 'engine.log'}")

    output = run / "FreeEed-output/freeeed-output/fixture/output/results"
    with zipfile.ZipFile(output / "native1.zip") as archive:
        if archive.read("native/UPI_00001_fixture.txt") != source:
            raise RuntimeError("the exported original differs from the input")
        extracted = archive.read("text/UPI_00001_fixture.txt.txt")
        if b"Synthetic test document for FreeEed" not in extracted:
            raise RuntimeError("the exported document has no expected extracted text")
    with (output / "metadata1.csv").open(newline="") as file:
        rows = list(csv.DictReader(file, delimiter="|"))
    if (len(rows) != 1 or rows[0]["Hash"] != hashlib.md5(source).hexdigest()
            or rows[0]["processing_exception"]):
        raise RuntimeError("unexpected metadata or processing exception")
    print(f"Synthetic extraction, original and metadata verified: {run}")


if __name__ == "__main__":
    main()
