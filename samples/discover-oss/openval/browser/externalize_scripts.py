#!/usr/bin/env python3
"""Move Next's generated inline bootstrap scripts into same-origin files."""

import hashlib
from pathlib import Path
import re
import sys


SCRIPT = re.compile(r"<script(?P<attrs>[^>]*)>(?P<body>.*?)</script>", re.DOTALL | re.IGNORECASE)


def externalize(path: Path) -> int:
    text = path.read_text()
    directory = path.parent / "_next/static/ato-bootstrap"
    count = 0

    def replace(match):
        nonlocal count
        attrs, body = match.group("attrs"), match.group("body")
        if re.search(r"\bsrc\s*=", attrs, re.IGNORECASE) or not body.strip():
            return match.group(0)
        type_match = re.search(r'\btype\s*=\s*["\']([^"\']+)', attrs, re.IGNORECASE)
        if type_match and type_match.group(1).lower() not in ("text/javascript", "application/javascript"):
            return match.group(0)
        directory.mkdir(parents=True, exist_ok=True)
        name = hashlib.sha256(body.encode()).hexdigest() + ".js"
        (directory / name).write_text(body)
        count += 1
        return f'<script{attrs} src="/_next/static/ato-bootstrap/{name}"></script>'

    result = SCRIPT.sub(replace, text)
    if count == 0:
        raise ValueError(f"expected Next bootstrap scripts in {path}")
    path.write_text(result)
    return count


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: externalize_scripts.py STATIC_EXPORT_DIR")
    export = Path(sys.argv[1])
    print(f"Externalized {externalize(export / 'index.html')} bootstrap scripts")
