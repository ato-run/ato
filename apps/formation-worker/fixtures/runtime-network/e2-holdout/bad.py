import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Placeholder(BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"pending"
        self.send_response(404)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


ThreadingHTTPServer(("127.0.0.1", int(os.environ["ATO_ENDPOINT_APP_HTTP_PORT"])), Placeholder).serve_forever()
