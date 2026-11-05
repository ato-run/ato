#!/usr/bin/env python3
"""Prepare a narrow movement authorization fix for the pinned upstream file."""

import argparse
import hashlib
from pathlib import Path

UPSTREAM_SHA256 = "93f51b53d906dfd499e68002aaf9098f52d9d40e21e2d360018096f45f71547b"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("upstream", type=Path)
    parser.add_argument("output", type=Path, help="new file; never overwritten")
    args = parser.parse_args()
    content = args.upstream.read_bytes()
    if hashlib.sha256(content).hexdigest() != UPSTREAM_SHA256:
        raise ValueError("Unexpected upstream service order source")
    text = content.decode("utf-8")
    start = text.index("def record_product_movement(")
    end = text.index("\n\ndef update_product_movement_on_submit(", start)
    section = text[start:end]
    before = '\torder = frappe.get_doc("Service Order", service_order)\n\n\trow = {'
    after = '''\torder = frappe.get_doc("Service Order", service_order)
\torder.check_permission("write")
\tif order.service_request:
\t\ttry:
\t\t\tlinked_request = frappe.get_doc("Service Request", order.service_request)
\t\texcept frappe.DoesNotExistError:
\t\t\tlinked_request = None
\t\tif linked_request is not None:
\t\t\tlinked_request.check_permission("write")

\trow = {'''
    if section.count(before) != 1:
        raise ValueError("Unexpected movement function structure")
    updated = text[:start] + section.replace(before, after, 1) + text[end:]
    compile(updated, args.upstream.name, "exec")
    result = updated.encode("utf-8")
    with args.output.open("xb") as output:
        output.write(result)
    print(f"Prepared movement source sha256:{hashlib.sha256(result).hexdigest()}")


if __name__ == "__main__":
    main()
