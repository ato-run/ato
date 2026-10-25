#!/usr/bin/env python3
"""Observe a Node workload descended from the one owner-pinned Runtime.

The materializer may give an entrypoint its file ID, not its Source filename.
This owner-only observer therefore uses executable identity and ancestry.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import time


def process_stat(raw):
    fields = raw[raw.rindex(") ") + 2:].split()
    return {"state": fields[0], "ppid": int(fields[1]), "start_ticks": fields[19]}


def descends_from(pid, ancestor, proc=Path("/proc")):
    seen = set()
    while pid > 1 and pid not in seen:
        if pid == ancestor:
            return True
        seen.add(pid)
        try:
            pid = process_stat((proc / str(pid) / "stat").read_text())["ppid"]
        except (OSError, ValueError, IndexError):
            return False
    return False


def observe(runtime_pid, runtime_start_ticks, node, proc=Path("/proc")):
    if process_stat((proc / str(runtime_pid) / "stat").read_text())["start_ticks"] != runtime_start_ticks:
        raise ValueError("runtime_identity_changed")
    for process in proc.iterdir():
        if not process.name.isdecimal():
            continue
        pid = int(process.name)
        if pid == runtime_pid:
            continue
        try:
            state = process_stat((process / "stat").read_text())
            if state["state"] in ("Z", "X") or not descends_from(pid, runtime_pid, proc):
                continue
            executable = Path(os.readlink(process / "exe"))
            if executable.name not in {node.name, "node"}:
                continue
            expected_digest = hashlib.sha256(node.read_bytes()).hexdigest()
            # A mount namespace may expose /toolchain/bin/node instead of its
            # host path. Compare executable bytes through proc, not that name.
            if hashlib.sha256((process / "exe").read_bytes()).hexdigest() != expected_digest:
                continue
            # No env, credentials or argv are read or exported.
            return {"at_ms": int(time.time() * 1000), "pid": pid,
                    "runtime_pid": runtime_pid, "workload_start_ticks": state["start_ticks"],
                    "executable": str(executable), "node_sha256": expected_digest,
                    "workload_actually_started": True, "Source_filename_matching": False}
        except (OSError, ValueError, IndexError):
            continue
    return None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--owner-root", type=Path, required=True)
    parser.add_argument("--node", type=Path, required=True)
    parser.add_argument("--timeout-seconds", type=int, default=600)
    args = parser.parse_args()
    control = json.loads((args.owner_root / "runtime-control.json").read_text())
    marker = args.owner_root / "workload-started.json"
    end = time.monotonic() + args.timeout_seconds
    while time.monotonic() < end:
        proof = observe(control["pid"], control["start_ticks"], args.node)
        if proof:
            with marker.open("x") as staged:
                os.chmod(marker, 0o600)
                json.dump(proof, staged)
            return
        time.sleep(0.01)
    raise RuntimeError("workload_start_not_observed")


if __name__ == "__main__":
    main()
