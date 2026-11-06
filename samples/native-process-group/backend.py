"""Backend of the native process group: one Port, no dependencies."""

import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/":
            body = b"from-backend\n"
        elif self.path == "/healthz":
            body = b"backend-ok\n"
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, _format, *_args):
        pass


port = int(os.environ["ATO_ENDPOINT_BACKEND_HTTP_PORT"])
ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
