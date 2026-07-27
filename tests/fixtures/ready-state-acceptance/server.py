"""Guest fixture for the Ready-State acceptance E2E.

Serves the three things the E2E needs to attribute a response to a SPECIFIC
guest and to prove the exact-launch contract survived into it.

    GET /health                    -> 200 "ok"
    GET /echo-nonce?value=<nonce>  -> the nonce, verbatim
    GET /launch-evidence           -> JSON: argv, cwd, pid (as observed at start)

# Why the launch evidence is captured before anything else runs

`/proc/self/cmdline` is the vector the kernel was handed, and it is read as the
very first statement so nothing in this file can have altered it. `sys.argv` is
recorded alongside it as auxiliary evidence only: Python drops the interpreter
from `sys.argv`, so it can never show that `resolved_argv[0]` was honoured. Only
the full cmdline vector can, which is what the harness gates on.

The working directory is read twice for the same reason. `os.getcwd()` as the
first statement is where the process STARTED; `/proc/self/cwd` is where it IS. A
workload that `chdir`'d between them shows up as a disagreement rather than as a
pass.

# Why /echo-nonce exists

A nonce baked into the guest CANNOT distinguish a restored guest from the guest
it was captured from: restore resumes identical memory, so both answer the same.
The nonce here is REQUEST-scoped instead — the harness invents a fresh random
value per restore and requires it echoed back, which proves the responder is
alive and serving this request rather than a cache or a proxy. Attribution to
the restored guest comes from combining that with the harness's proof that the
held guest was gone and its address was dead in the gap.
"""

import json
import os
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import parse_qs, urlparse


def _read_cmdline():
    """The kernel's argv vector, NUL-delimited AND NUL-terminated.

    Exactly one trailing empty piece is the terminator; every other one is a
    real empty argument. Dropping them all would make ["a", "", "b"] and
    ["a", "b"] indistinguishable, so exactly one is removed.
    """
    try:
        with open("/proc/self/cmdline", "rb") as handle:
            raw = handle.read()
    except OSError:
        return None
    parts = raw.split(b"\x00")
    if parts and parts[-1] == b"":
        parts.pop()
    return [p.decode("utf-8", "surrogateescape") for p in parts]


# Captured as the FIRST thing this process does, before any handler can run.
LAUNCH_EVIDENCE = {
    "proc_cmdline": _read_cmdline(),
    "sys_argv": list(sys.argv),
    "initial_getcwd": os.getcwd(),
    "proc_self_cwd": os.path.realpath("/proc/self/cwd"),
    "pid": os.getpid(),
}


class Handler(BaseHTTPRequestHandler):
    def _send(self, code, body, content_type="text/plain; charset=utf-8"):
        payload = body.encode("utf-8") if isinstance(body, str) else body
        self.send_response(code)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        parsed = urlparse(self.path)
        if parsed.path == "/health":
            self._send(200, "ok")
        elif parsed.path == "/echo-nonce":
            values = parse_qs(parsed.query).get("value", [])
            if not values or not values[0]:
                self._send(400, "missing value")
            else:
                # Verbatim, with no framing of our own: the harness compares the
                # whole body, so any decoration here would read as a mismatch.
                self._send(200, values[0])
        elif parsed.path == "/launch-evidence":
            self._send(
                200,
                json.dumps(LAUNCH_EVIDENCE, ensure_ascii=False),
                "application/json; charset=utf-8",
            )
        else:
            self._send(404, "not found")

    def log_message(self, *_args):
        # The guest console is boot evidence; per-request noise buries it.
        pass


if __name__ == "__main__":
    HTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
