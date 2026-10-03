#!/usr/bin/env python3
"""Owner-only canary scan: values never enter model tools or diagnostics."""
import argparse
import base64
import hashlib
import json
from pathlib import Path
from urllib.parse import quote


def variants(value):
    if not isinstance(value, str) or len(value) < 16:
        raise ValueError("private_canary_bounds")
    return {value.encode(), json.dumps(value, ensure_ascii=True)[1:-1].encode(),
            quote(value, safe="").encode(), base64.b64encode(value.encode())}


def scan(transcripts, canaries):
    needles = set().union(*(variants(value) for value in canaries))
    records = []
    for path in transcripts:
        path = Path(path)
        if path.is_symlink() or not path.is_file():
            raise ValueError("regular_transcript_required")
        data = path.read_bytes()
        # Both arguments AND tool results, native errors and agent text are
        # covered by the entire SDK stream, rather than a Producer-only view.
        records.append({"transcript": path.name, "bytes": len(data),
                        "sha256": hashlib.sha256(data).hexdigest(),
                        "private_value_detected": any(n in data for n in needles)})
    return {"schema": "ato.formation-agent-output-leak-scan/1",
            "canary_count": len(canaries), "records": records,
            "leak_detected": any(r["private_value_detected"] for r in records),
            "scope": "supplied private canaries; not proof of every possible secret",
            "private_values_or_paths_reported": False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--private-identity", type=Path, required=True)
    parser.add_argument("--private-value-file", type=Path, action="append", default=[])
    parser.add_argument("--transcript", type=Path, action="append", required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    args = parser.parse_args()
    try:
        canaries = [json.loads(args.private_identity.read_text())["email"]]
        canaries.extend(p.read_text().strip() for p in args.private_value_file)
        report = scan(args.transcript, canaries)
        with args.evidence.open("x") as file:
            json.dump(report, file, indent=2)
            file.write("\n")
        print(json.dumps({"leak_detected": report["leak_detected"],
                          "transcripts": len(report["records"])}))
        return int(report["leak_detected"])
    except (OSError, ValueError, KeyError):
        # No path, value, parser excerpt or raw native error in diagnostics.
        print(json.dumps({"error": "owner_leak_scan_failed"}))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
