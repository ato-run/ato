from http.server import BaseHTTPRequestHandler, HTTPServer

HTML = """
<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>Python Backend + HTML</title>
  </head>
  <body>
    <h1>Hello from Python backend</h1>
    <p>Static HTML served by http.server</p>
  </body>
</html>
"""

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.end_headers()
        self.wfile.write(HTML.encode("utf-8"))

    def log_message(self, format, *args):
        return

if __name__ == "__main__":
    server = HTTPServer(("127.0.0.1", 8080), Handler)
    print("Serving on http://127.0.0.1:8080")
    server.serve_forever()
