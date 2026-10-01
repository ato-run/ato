# Reports the effective identity, working directory and whether the image's
# VOLUME path (relative to the working directory) is writable.
import http.server
import json
import os

try:
    with open("server-data/probe", "w") as probe:
        probe.write("written by the route\n")
    WRITABLE = True
except OSError:
    WRITABLE = False
WHOAMI = json.dumps(
    {"cwd": os.getcwd(), "gid": os.getgid(), "state_writable": WRITABLE, "uid": os.getuid()},
    sort_keys=True,
).encode()


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = WHOAMI if self.path == "/whoami" else b"ok\n"
        self.send_response(200)
        self.send_header("content-type", "application/json" if self.path == "/whoami" else "text/plain")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


http.server.ThreadingHTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
