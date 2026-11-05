#!/usr/bin/env python3
"""Prepare a narrow schedule list authorization fix for the pinned source."""

import argparse
import hashlib
from pathlib import Path

UPSTREAM_SHA256 = "7003d7b64bac50f13c10c4247cd4d11024864e614d925c1e8cfe43a596c4b2df"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("upstream", type=Path)
    parser.add_argument("output", type=Path, help="new file; never overwritten")
    args = parser.parse_args()
    content = args.upstream.read_bytes()
    if hashlib.sha256(content).hexdigest() != UPSTREAM_SHA256:
        raise ValueError("Unexpected upstream schedule source")
    text = content.decode("utf-8")
    old = '\torders = frappe.get_all(\n\t\t"Service Order",'
    new = '\torders = frappe.get_list(\n\t\t"Service Order",'
    if text.count(old) != 1:
        raise ValueError("Unexpected schedule list structure")
    text = text.replace(old, new, 1)
    compile(text, args.upstream.name, "exec")
    result = text.encode("utf-8")
    with args.output.open("xb") as output:
        output.write(result)
    print(f"Prepared schedule source sha256:{hashlib.sha256(result).hexdigest()}")


if __name__ == "__main__":
    main()
