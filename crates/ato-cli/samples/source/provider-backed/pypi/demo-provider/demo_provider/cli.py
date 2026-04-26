from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("input_path")
    parser.add_argument("-o", "--output", required=True)
    args = parser.parse_args()

    helper_available = False
    try:
        import demo_provider_pdf_helper

        helper_available = bool(getattr(demo_provider_pdf_helper, "PDF_HELPER", False))
    except ImportError:
        helper_available = False

    input_path = Path(args.input_path)
    output_path = Path(args.output)
    payload = {
        "cwd": os.getcwd(),
        "argv": [args.input_path, "-o", args.output],
        "input_exists": input_path.exists(),
        "content": input_path.read_text(encoding="utf-8"),
        "helper_available": helper_available,
        "python_version": list(sys.version_info[:3]),
        "python_executable": sys.executable,
    }
    output_path.write_text(json.dumps(payload, ensure_ascii=True), encoding="utf-8")
    print(json.dumps(payload, ensure_ascii=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())