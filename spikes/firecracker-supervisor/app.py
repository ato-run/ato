#!/usr/bin/env python3
"""Supervisor-capsule test workload.

Reads OPENAI_API_KEY from the process ENVIRONMENT (not a file) — this is the
`delivery = "env"` case the supervisor exists for. Health is gated on the key
being present, so /health only comes up once the guest-agent has (re)started
this process with the composed env. /keyhash echoes a short SHA-256 prefix of
the key so a test can prove WHICH key is live without leaking the value.
"""
import hashlib
import http.server
import os


class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        key = os.environ.get("OPENAI_API_KEY", "")
        if self.path == "/health":
            code = 200 if key else 503
            self.send_response(code)
            self.end_headers()
            self.wfile.write(b"ok" if key else b"no-key")
        elif self.path == "/keyhash":
            self.send_response(200)
            self.end_headers()
            digest = hashlib.sha256(key.encode()).hexdigest()[:12] if key else "none"
            self.wfile.write(digest.encode())
        else:
            self.send_response(404)
            self.end_headers()

    def log_message(self, *a):
        pass


http.server.HTTPServer(("0.0.0.0", 8080), H).serve_forever()
