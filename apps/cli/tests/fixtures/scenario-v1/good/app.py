"""Scenario verifier v1 fixture: a stateful JSON app on the Python stdlib.

The one identity this fixture pins is the body of the `update` step:

    {"id":"item-1","name":"updated"}

The item id is assigned deterministically per fresh process, and the Scenario
captures it, so the transcript digest is stable across runs.

The listen port comes from the Runtime's ATO_ENDPOINT_APP_HTTP_PORT injection.
"""

import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = b'{"service":"scenario-v1","status":"ok"}\n'
SESSION = "session=ok"
ITEMS = {}
DELETED = set()
NEXT_ID = [1]


def authorized(handler):
    return SESSION in (handler.headers.get("Cookie") or "")


class Handler(BaseHTTPRequestHandler):
    server_version = "scenario-v1-good/1"

    def reply(self, status, payload=None, cookie=False):
        body = b"" if payload is None else json.dumps(
            payload, separators=(",", ":")
        ).encode("utf-8")
        self.send_response(status)
        if cookie:
            self.send_header("Set-Cookie", SESSION + "; Path=/")
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if body:
            self.wfile.write(body)

    def do_GET(self):  # noqa: N802 - BaseHTTPRequestHandler's API
        if self.path == "/":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(ROOT)))
            self.end_headers()
            self.wfile.write(ROOT)
            return
        if self.path.startswith("/items/"):
            item_id = self.path[len("/items/"):]
            if not authorized(self):
                self.reply(401, {"error": "unauthorized"})
                return
            if item_id in DELETED or item_id not in ITEMS:
                self.reply(404, {"error": "not_found"})
                return
            self.reply(200, {"id": item_id, "name": ITEMS[item_id]})
            return
        self.reply(404, {"error": "not_found"})

    def do_POST(self):  # noqa: N802
        if self.path == "/login":
            self.reply(200, {"logged_in": True}, cookie=True)
            return
        if self.path == "/items":
            if not authorized(self):
                self.reply(401, {"error": "unauthorized"})
                return
            item_id = "item-%d" % NEXT_ID[0]
            NEXT_ID[0] += 1
            ITEMS[item_id] = "first"
            self.reply(201, {"id": item_id, "name": ITEMS[item_id]})
            return
        self.reply(404, {"error": "not_found"})

    def do_PUT(self):  # noqa: N802
        if self.path.startswith("/items/"):
            item_id = self.path[len("/items/"):]
            if not authorized(self):
                self.reply(401, {"error": "unauthorized"})
                return
            if item_id in DELETED or item_id not in ITEMS:
                self.reply(404, {"error": "not_found"})
                return
            ITEMS[item_id] = "updated"
            self.reply(200, {"id": item_id, "name": ITEMS[item_id]})
            return
        self.reply(404, {"error": "not_found"})

    def do_DELETE(self):  # noqa: N802
        if self.path.startswith("/items/"):
            item_id = self.path[len("/items/"):]
            if not authorized(self):
                self.reply(401, {"error": "unauthorized"})
                return
            if item_id in DELETED or item_id not in ITEMS:
                self.reply(404, {"error": "not_found"})
                return
            DELETED.add(item_id)
            self.reply(204)
            return
        self.reply(404, {"error": "not_found"})

    def log_message(self, format, *args):  # noqa: A002 - stdlib signature
        pass


def main():
    port = int(os.environ.get("ATO_ENDPOINT_APP_HTTP_PORT", "8000"))
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()


if __name__ == "__main__":
    main()
