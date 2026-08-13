import http.server


class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"ok")

    def log_message(self, *a):
        pass


# Deliberately loopback-only: the TAP-side boot-verify must never reach this.
http.server.HTTPServer(("127.0.0.1", 8080), H).serve_forever()
