import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path != "/health":
            self.send_error(404)
            return
        if not os.environ.get("ATO_BINDING_SERVICE"):
            self.send_error(503)
            return
        body = b"binding-ok\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, _format, *_args):
        pass


port = int(os.environ.get("ATO_ENDPOINT_APP_HTTP", "8000"))
ThreadingHTTPServer(("0.0.0.0", port), Handler).serve_forever()
