import os
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer


class Handler(SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/health":
            body = b"ok"
            self.send_response(200)
            self.send_header("content-type", "text/plain")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        super().do_GET()

    def log_message(self, *args):
        pass


os.chdir(os.path.dirname(os.path.abspath(__file__)))
port = int(os.environ["ATO_ENDPOINT_APP_HTTP_PORT"])
ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
