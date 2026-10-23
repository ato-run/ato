"""Standard-library fixture: component health and a real sibling-service call."""
import os
import sys
import urllib.request
from http.server import BaseHTTPRequestHandler, HTTPServer

role, port = sys.argv[1], int(sys.argv[2])


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if role == "backend":
            status = 503 if os.environ.get("FAIL_HEALTH") == "1" else 200
            body = b"backend-ok\n" if status == 200 else b"backend-down\n"
        elif self.path == "/joined":
            try:
                opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
                with opener.open("http://backend:8091/health", timeout=2) as response:
                    healthy = response.status == 200 and response.read(64) == b"backend-ok\n"
                status, body = (200, b"joined-ok\n") if healthy else (502, b"dependency-down\n")
            except Exception:
                status, body = 502, b"dependency-down\n"
        else:
            status, body = 200, b"web-ok\n"
        self.send_response(status)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


HTTPServer(("0.0.0.0", port), Handler).serve_forever()
