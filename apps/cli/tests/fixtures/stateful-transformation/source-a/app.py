"""Stateful transformation source A: an in-memory JSON store (stdlib only).

The observable HTTP behavior is the contract; the storage is internal. This
implementation keeps the whole store in process memory with this shape:

    {"next_id": 1, "users": [...], "items": {"item-1": {"id", "name", "quantity"}}}

A fresh Run starts a fresh process, so the scenario's `fresh_instance` isolation
is satisfied without a state slot or a writable filesystem. Source B uses an
in-memory SQLite database with a different schema.
"""

import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

STORE = {"next_id": 1, "users": [], "items": {}}
OK = b'{"service":"stateful-transformation","status":"ok"}\n'
SESSION = "session=ok"


def load():
    return STORE


def save(store):
    # The in-memory store is the object `load` returned; updating it keeps
    # mutations that already happened on it and stays a single source of truth.
    STORE.update(store)


def authorized(handler):
    return SESSION in (handler.headers.get("Cookie") or "")


def item_body(item_id, name, quantity):
    return ('{"id":"%s","name":"%s","quantity":%d}' % (item_id, name, quantity)).encode()


class Handler(BaseHTTPRequestHandler):
    server_version = "stateful-transformation-a/1"

    def reply(self, status, payload=None, cookie=False):
        body = b"" if payload is None else payload
        self.send_response(status)
        if cookie:
            self.send_header("Set-Cookie", SESSION + "; Path=/")
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if body:
            self.wfile.write(body)

    def read_json(self):
        length = int(self.headers.get("Content-Length") or "0")
        raw = self.rfile.read(length) if length else b"{}"
        return json.loads(raw.decode("utf-8"))

    def do_GET(self):  # noqa: N802
        if self.path == "/":
            self.reply(200, OK)
            return
        if self.path.startswith("/items/"):
            if not authorized(self):
                self.reply(401, b'{"error":"unauthorized"}')
                return
            item_id = self.path[len("/items/"):]
            item = load()["items"].get(item_id)
            if item is None:
                self.reply(404, b'{"error":"not_found"}')
                return
            self.reply(200, item_body(item["id"], item["name"], item["quantity"]))
            return
        self.reply(404, b'{"error":"not_found"}')

    def do_POST(self):  # noqa: N802
        if self.path == "/login":
            store = load()
            if not any(user["username"] == "tester" for user in store["users"]):
                store["users"].append({"username": "tester"})
                save(store)
            self.reply(200, b'{"logged_in":true}', cookie=True)
            return
        if self.path == "/items":
            if not authorized(self):
                self.reply(401, b'{"error":"unauthorized"}')
                return
            try:
                payload = self.read_json()
                name = payload["name"]
                quantity = payload["quantity"]
            except (ValueError, KeyError, TypeError):
                self.reply(400, b'{"error":"invalid_item"}')
                return
            if not isinstance(name, str) or not name or not isinstance(quantity, int) or quantity < 0:
                self.reply(400, b'{"error":"invalid_item"}')
                return
            store = load()
            item_id = "item-%d" % store["next_id"]
            store["next_id"] += 1
            store["items"][item_id] = {"id": item_id, "name": name, "quantity": quantity}
            save(store)
            self.reply(201, item_body(item_id, name, quantity))
            return
        self.reply(404, b'{"error":"not_found"}')

    def do_PUT(self):  # noqa: N802
        if not self.path.startswith("/items/"):
            self.reply(404, b'{"error":"not_found"}')
            return
        if not authorized(self):
            self.reply(401, b'{"error":"unauthorized"}')
            return
        item_id = self.path[len("/items/"):]
        store = load()
        if item_id not in store["items"]:
            self.reply(404, b'{"error":"not_found"}')
            return
        try:
            payload = self.read_json()
            name = payload["name"]
            quantity = payload["quantity"]
        except (ValueError, KeyError, TypeError):
            self.reply(400, b'{"error":"invalid_item"}')
            return
        if not isinstance(name, str) or not name or not isinstance(quantity, int) or quantity < 0:
            self.reply(400, b'{"error":"invalid_item"}')
            return
        store["items"][item_id] = {"id": item_id, "name": name, "quantity": quantity}
        save(store)
        self.reply(200, item_body(item_id, name, quantity))

    def do_DELETE(self):  # noqa: N802
        if not self.path.startswith("/items/"):
            self.reply(404, b'{"error":"not_found"}')
            return
        if not authorized(self):
            self.reply(401, b'{"error":"unauthorized"}')
            return
        item_id = self.path[len("/items/"):]
        store = load()
        if item_id not in store["items"]:
            self.reply(404, b'{"error":"not_found"}')
            return
        del store["items"][item_id]
        save(store)
        self.reply(204)

    def log_message(self, format, *args):  # noqa: A002
        pass


def main():
    port = int(os.environ.get("ATO_ENDPOINT_APP_HTTP_PORT", "8000"))
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()


if __name__ == "__main__":
    main()
