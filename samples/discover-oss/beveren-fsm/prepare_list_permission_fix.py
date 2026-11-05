#!/usr/bin/env python3
"""Prepare two permission-aware Beveren list endpoints for local verification.

This does not make the rest of Beveren safe to expose. Inputs are restricted to
the inspected upstream commit; output is a new directory, never an in-place edit.
"""

import argparse
import hashlib
from pathlib import Path

FILES = {
    "service_request.py": "355bece45c72b2b349b4c2ebce24c26c261581dcdc0124d103737eeca3bca983",
    "service_order.py": "1d5f13c8082717624e95d58c5a95c331392106dc92357e4a4b1d5d8eff93be52",
}


def replace_once(text: str, old: str, new: str) -> str:
    if text.count(old) != 1:
        raise ValueError("Unexpected source structure")
    return text.replace(old, new, 1)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("upstream_api", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    patched = {}
    for name, digest in FILES.items():
        content = (args.upstream_api / name).read_bytes()
        if hashlib.sha256(content).hexdigest() != digest:
            raise ValueError(f"Unexpected upstream digest for {name}")
        text = content.decode("utf-8")
        if name == "service_request.py":
            text = replace_once(text, "requests = frappe.get_all(", "requests = frappe.get_list(")
            text = replace_once(text, "if request_names:\n", 'if request_names and frappe.has_permission("Service Order", "read"):\n')
            text = replace_once(text, "related_orders = frappe.get_all(", "related_orders = frappe.get_list(")
        else:
            text = replace_once(text, "orders = frappe.get_all(", "orders = frappe.get_list(")
        compile(text, name, "exec")
        patched[name] = text.encode("utf-8")
    args.output.mkdir(exist_ok=False)
    for name, content in patched.items():
        with (args.output / name).open("xb") as output:
            output.write(content)
        print(f"{name} sha256:{hashlib.sha256(content).hexdigest()}")


if __name__ == "__main__":
    main()
