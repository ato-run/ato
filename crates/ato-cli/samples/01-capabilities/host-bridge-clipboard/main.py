#!/usr/bin/env python3
"""
host-bridge-clipboard: demonstrates IPC bridge + capability gate.

Reads the clipboard via the host bridge, appends a timestamp, then writes
back. In ato-desktop, this triggers a consent dialog on first run.

Outside ato-desktop (no IPC proxy), the IPC calls return an error and the
script exits non-zero — demonstrating that capabilities are unavailable when
the bridge is absent.
"""

import json
import os
import sys
from datetime import datetime

IPC_SOCKET = os.environ.get("ATO_IPC_SOCKET", "")


def bridge_invoke(command: str, capability: str, payload: dict) -> dict:
    """Send a JSON-RPC request to the ato host bridge over the IPC socket."""
    if not IPC_SOCKET:
        raise RuntimeError(
            "ATO_IPC_SOCKET not set — ato-desktop bridge is required. "
            "Run this capsule inside ato-desktop, or set ATO_IPC_SOCKET."
        )

    import socket

    request = json.dumps(
        {
            "kind": "invoke",
            "request_id": 1,
            "command": command,
            "capability": capability,
            "payload": payload,
        }
    )

    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.connect(IPC_SOCKET)
        sock.sendall(request.encode() + b"\n")
        raw = b""
        while True:
            chunk = sock.recv(4096)
            if not chunk:
                break
            raw += chunk
            if b"\n" in raw:
                break

    return json.loads(raw.strip())


def main() -> int:
    print("host-bridge-clipboard: reading clipboard via ato host bridge")

    try:
        read_resp = bridge_invoke("read", "clipboard.read", {})
    except RuntimeError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1

    if read_resp.get("status") == "denied":
        print(f"DENIED: clipboard.read was not granted: {read_resp.get('message')}")
        return 1
    if read_resp.get("status") != "ok":
        print(f"ERROR: unexpected response: {read_resp}", file=sys.stderr)
        return 1

    original = read_resp.get("payload", {}).get("text", "")
    print(f"clipboard read: {original!r}")

    new_text = f"{original}\n[ato capsule @ {datetime.now().isoformat()}]"

    write_resp = bridge_invoke("write", "clipboard.write", {"text": new_text})
    if write_resp.get("status") == "denied":
        print(f"DENIED: clipboard.write was not granted: {write_resp.get('message')}")
        return 1
    if write_resp.get("status") != "ok":
        print(f"ERROR: write failed: {write_resp}", file=sys.stderr)
        return 1

    print("clipboard updated successfully")
    print(f"new content: {new_text!r}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
