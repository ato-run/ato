"""Minimal stdlib HTTP server for the source-python-runtime-tools fixture.

Serves the built dist/ directory at / and exposes a JSON health endpoint at
/api/health.  Auto-shuts down after 30 seconds so the test process exits
without needing an external kill signal.
"""
import http.server
import json
import os
import threading


class _Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory="dist", **kwargs)

    def do_GET(self):
        if self.path == "/api/health":
            body = json.dumps({"status": "ok"}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            super().do_GET()

    def log_message(self, *args):
        pass


port = int(os.environ.get("PORT", "0"))
server = http.server.HTTPServer(("127.0.0.1", port), _Handler)
# Auto-shutdown so the ato process (and the test) can exit cleanly.
threading.Timer(30.0, server.shutdown).start()
server.serve_forever()
