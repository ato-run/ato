"""Preregistered negative control: the same K's HTTP observation must FAIL."""
import os
import oss_http

class Wrong(oss_http.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_error(404)

oss_http.ThreadingHTTPServer(("127.0.0.1", int(os.environ["ATO_ENDPOINT_APP_HTTP_PORT"])), Wrong).serve_forever()
