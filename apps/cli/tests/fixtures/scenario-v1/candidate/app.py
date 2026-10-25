"""Scenario verifier v1 candidate: a second implementation of the same app.

Deliberately a different source from `good/` (different structure and response
helpers) that must produce the same Contract-visible behavior. The body the
`update` step pins is byte-identical:

    {"id":"item-1","name":"updated"}
"""

import json
import os
from http.server import BaseHTTPRequestHandler, HTTPServer

SERVICE = b'{"service":"scenario-v1","status":"ok"}\n'
COOKIE = "session=ok"
store = {}
gone = set()
sequence = [0]


def encode(payload):
    if payload is None:
        return b""
    return json.dumps(payload, separators=(",", ":")).encode("utf-8")


def session_from(handler):
    return COOKIE in (handler.headers.get("Cookie") or "")


class ApiHandler(BaseHTTPRequestHandler):
    server_version = "scenario-v1-candidate/1"

    def emit(self, code, payload=None, set_cookie=False):
        body = encode(payload)
        self.send_response(code)
        if set_cookie:
            self.send_header("Set-Cookie", COOKIE + "; Path=/")
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if body:
            self.wfile.write(body)

    def do_GET(self):  # noqa: N802
        if self.path == "/":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(SERVICE)))
            self.end_headers()
            self.wfile.write(SERVICE)
        elif self.path.startswith("/items/"):
            item = self.path.split("/", 2)[2]
            if not session_from(self):
                self.emit(401, {"error": "unauthorized"})
            elif item in gone or item not in store:
                self.emit(404, {"error": "not_found"})
            else:
                self.emit(200, {"id": item, "name": store[item]})
        else:
            self.emit(404, {"error": "not_found"})

    def do_POST(self):  # noqa: N802
        if self.path == "/login":
            self.emit(200, {"logged_in": True}, set_cookie=True)
        elif self.path == "/items":
            if not session_from(self):
                self.emit(401, {"error": "unauthorized"})
            else:
                sequence[0] += 1
                item = "item-%d" % sequence[0]
                store[item] = "first"
                self.emit(201, {"id": item, "name": store[item]})
        else:
            self.emit(404, {"error": "not_found"})

    def do_PUT(self):  # noqa: N802
        if not self.path.startswith("/items/"):
            self.emit(404, {"error": "not_found"})
            return
        item = self.path.split("/", 2)[2]
        if not session_from(self):
            self.emit(401, {"error": "unauthorized"})
        elif item in gone or item not in store:
            self.emit(404, {"error": "not_found"})
        else:
            store[item] = "updated"
            self.emit(200, {"id": item, "name": store[item]})

    def do_DELETE(self):  # noqa: N802
        if not self.path.startswith("/items/"):
            self.emit(404, {"error": "not_found"})
            return
        item = self.path.split("/", 2)[2]
        if not session_from(self):
            self.emit(401, {"error": "unauthorized"})
        elif item in gone or item not in store:
            self.emit(404, {"error": "not_found"})
        else:
            gone.add(item)
            self.emit(204)

    def log_message(self, format, *args):  # noqa: A002
        pass


def main():
    port = int(os.environ.get("ATO_ENDPOINT_APP_HTTP_PORT", "8000"))
    HTTPServer(("127.0.0.1", port), ApiHandler).serve_forever()


if __name__ == "__main__":
    main()
