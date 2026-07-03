import http.server

import fakelib


class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"pem-detect:" + str(fakelib.looks_like_pem("x")).encode())

    def log_message(self, *a):
        pass


http.server.HTTPServer(("0.0.0.0", 8080), H).serve_forever()
