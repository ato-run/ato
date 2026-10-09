#!/usr/bin/env python3
"""Exercise the worker's real HTTP claim loop without launching a workload.

Usage: python3 tests/manual/runner_polling.py path/to/ato-connected-realization-worker
Only a loopback fake control plane is contacted. Each case gets a fresh work root.
"""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def measure(binary, response_delay, once=False):
    claims = []
    replies = []
    paths = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def reply(self, status, body):
            data = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            self.wfile.flush()

        def do_POST(self):
            self.rfile.read(int(self.headers.get("Content-Length", "0")))
            paths.append(self.path)
            self.reply(200 if self.path.endswith("/heartbeat") else 400, {})

        def do_GET(self):
            paths.append(self.path)
            if self.path.endswith("/leases/open"):
                self.reply(200, {"leases": []})
            elif self.path.endswith("/leases/next?wait_ms=20000"):
                claims.append(time.monotonic())
                if len(claims) == 1:
                    time.sleep(response_delay)
                    self.reply(200, {"lease": None, "next_poll_seconds": 1})
                    replies.append(time.monotonic())
                else:
                    # A permanent error terminates the real loop without
                    # credentials, lease creation, or a workload.
                    self.reply(401, {})
            else:
                self.reply(400, {})

    temporary = Path(".tmp")
    temporary.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="runner-poll-", dir=temporary) as root:
        with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
            server_thread = threading.Thread(target=server.serve_forever, daemon=True)
            server_thread.start()
            try:
                args = [
                    str(binary), "--api-base", f"http://127.0.0.1:{server.server_port}",
                    "--runner-id", "poll-regression-runner", "--runner-token", "test-only",
                    "--work-root", str(Path(root).resolve()),
                    "--surface-target", "127.0.0.1:18080",
                ]
                if once:
                    args.append("--once")
                env = {key: value for key, value in os.environ.items() if not key.startswith("ATO_")}
                env["TMPDIR"] = str(Path(root).resolve())
                # No workload is launched. Keep capability/recovery probes
                # away from the developer's Docker daemon and other runtimes.
                env["PATH"] = str(Path(root).resolve())
                result = subprocess.run(args, env=env, capture_output=True, text=True, timeout=45)
                assert result.returncode == (0 if once else 1), result.stderr
                assert len(claims) == (1 if once else 2), (paths, result.stderr)
                assert replies, (paths, result.stderr)
                return {
                    "response_delay_ms": round(response_delay * 1000),
                    "once": once,
                    "claims": len(claims),
                    "idle_gap_ms": round((claims[1] - replies[0]) * 1000) if not once else None,
                    "poll_interval_ms": round((claims[1] - claims[0]) * 1000) if not once else None,
                }
            finally:
                server.shutdown()
                server_thread.join()


if __name__ == "__main__":
    worker = Path(sys.argv[1]).resolve(strict=True)
    long_poll = measure(worker, 1.2)
    fast_empty = measure(worker, 0)
    one_shot = measure(worker, 0, once=True)
    print(json.dumps([long_poll, fast_empty, one_shot], indent=2), flush=True)
    assert long_poll["idle_gap_ms"] < 800, "extra idle gap after completed long poll"
    assert 950 <= fast_empty["poll_interval_ms"] < 2500, "fast empty claims lost their rate limit"
