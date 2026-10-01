import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Impl(BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"ok" if self.path == "/health" else b"impl"
        self.send_response(200)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", int(os.environ["ATO_ENDPOINT_APP_HTTP_PORT"])), Impl).serve_forever()
