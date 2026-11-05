#!/usr/bin/env python3
"""Adapt the pinned FreeEed engine to an explicitly configured Tika endpoint."""

import hashlib
from pathlib import Path
import sys


def replace_checked(path: Path, expected_sha256: str, old: str, new: str) -> None:
    data = path.read_bytes()
    if hashlib.sha256(data).hexdigest() != expected_sha256:
        raise ValueError(f"unexpected upstream source: {path}")
    text = data.decode("utf-8")
    if text.count(old) != 1:
        raise ValueError(f"expected exactly one replacement in {path}")
    path.write_text(text.replace(old, new), encoding="utf-8")


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit("usage: patch_upstream.py FREEEED_SOURCE_ROOT")
    root = Path(sys.argv[1]) / "freeeed-processing/src/main/java/org/freeeed"
    tika = root / "api/tika/TikaRestApi.java"
    parameters = root / "main/ParameterProcessing.java"
    # Check both inputs before changing either one.
    expected = {
        tika: "f2c16bdc39bf89f06badbca2b48164fd10a2270eebad1569e743fe8f5d3ff01c",
        parameters: "afbb2093537893e4644a87382ca3c0e78c9ec5b090100338a5a8b1bdcdfb3871",
    }
    for path, digest in expected.items():
        if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
            raise ValueError(f"unexpected upstream source: {path}")
    replace_checked(
        tika, expected[tika],
        '    static String TIKA_URL = "http://localhost:9998/tika";\n'
        '    static String METADATA_URL = "http://localhost:9998/meta";\n'
        '    static String LANGUAGE_URL = "http://localhost:9998/language/string";',
        '    static String TIKA_BASE = System.getProperty("freeeed.tika.base", "http://localhost:9998");\n'
        '    static String TIKA_URL = TIKA_BASE + "/tika";\n'
        '    static String METADATA_URL = TIKA_BASE + "/meta";\n'
        '    static String LANGUAGE_URL = TIKA_BASE + "/language/string";',
    )
    # helloTika is diagnostic, but it must refer to the same configured server.
    text = tika.read_text(encoding="utf-8")
    old = '        String url = "http://localhost:9998/tika";'
    if text.count(old) != 1:
        raise ValueError("unexpected helloTika implementation")
    tika.write_text(text.replace(old, "        String url = TIKA_URL;"), encoding="utf-8")
    replace_checked(
        parameters, expected[parameters],
        'public static final String TMP_DIR = "/tmp" + File.separator;',
        'public static final String TMP_DIR = System.getProperty("java.io.tmpdir") + File.separator;',
    )


if __name__ == "__main__":
    main()
