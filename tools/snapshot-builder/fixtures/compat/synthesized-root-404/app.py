import http.server


class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        # Only /health answers 200; the synthesized probe GETs "/" and sees 404.
        if self.path == "/health":
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"ok")
        else:
            self.send_response(404)
            self.end_headers()

    def log_message(self, *a):
        pass


http.server.HTTPServer(("0.0.0.0", 8080), H).serve_forever()
