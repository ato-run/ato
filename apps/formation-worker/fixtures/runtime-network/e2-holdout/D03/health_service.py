import os
from http.server import BaseHTTPRequestHandler, HTTPServer


class ImplHealth(BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"ok"
        self.send_response(200)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


HTTPServer(("127.0.0.1", int(os.environ["ATO_ENDPOINT_APP_HTTP_PORT"])), ImplHealth).serve_forever()
