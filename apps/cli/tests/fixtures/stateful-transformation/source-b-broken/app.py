"""Stateful transformation source B (broken behavior: authorization is ignored).

Same observable HTTP behavior as source A, different implementation and data
model. The SQLite database is in process memory (a fresh Run is a fresh process, so
the scenario's fresh_instance isolation needs no state slot or writable
filesystem) and uses:

    users(id INTEGER PRIMARY KEY AUTOINCREMENT, username TEXT, created_at TEXT)
    items(sku TEXT PRIMARY KEY, name TEXT, qty INTEGER, created_at TEXT)

Source A stores the same entities as a JSON object with `id/name/quantity`
fields. The scenario (which only sees HTTP) cannot tell the two schemas apart.
"""

import json
import os
import sqlite3
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

DB = ":memory:"
OK = b'{"service":"stateful-transformation","status":"ok"}\n'
SESSION = "session=ok"
CREATED_AT = "1970-01-01T00:00:00Z"
DDL = """
CREATE TABLE IF NOT EXISTS users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    username TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS items (
    sku TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    qty INTEGER NOT NULL,
    created_at TEXT NOT NULL
);
"""


_CONNECTION = sqlite3.connect(DB, check_same_thread=False)
_CONNECTION.executescript(DDL)
_CONNECTION.commit()


def connect():
    return _CONNECTION


def authorized(handler):
    # Broken on purpose: the cleared-cookie negative case must fail.
    return True


def item_body(sku, name, qty):
    return ('{"id":"%s","name":"%s","quantity":%d}' % (sku, name, qty)).encode()


class Handler(BaseHTTPRequestHandler):
    server_version = "stateful-transformation-b/1"

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
            sku = self.path[len("/items/"):]
            with connect() as connection:
                row = connection.execute(
                    "SELECT name, qty FROM items WHERE sku = ?", (sku,)
                ).fetchone()
            if row is None:
                self.reply(404, b'{"error":"not_found"}')
                return
            self.reply(200, item_body(sku, row[0], row[1]))
            return
        self.reply(404, b'{"error":"not_found"}')

    def do_POST(self):  # noqa: N802
        if self.path == "/login":
            with connect() as connection:
                exists = connection.execute(
                    "SELECT 1 FROM users WHERE username = ?", ("tester",)
                ).fetchone()
                if exists is None:
                    connection.execute(
                        "INSERT INTO users (username, created_at) VALUES (?, ?)",
                        ("tester", CREATED_AT),
                    )
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
            with connect() as connection:
                count = connection.execute("SELECT COUNT(*) FROM items").fetchone()[0]
                sku = "item-%d" % (count + 1)
                connection.execute(
                    "INSERT INTO items (sku, name, qty, created_at) VALUES (?, ?, ?, ?)",
                    (sku, name, quantity, CREATED_AT),
                )
            self.reply(201, item_body(sku, name, quantity))
            return
        self.reply(404, b'{"error":"not_found"}')

    def do_PUT(self):  # noqa: N802
        if not self.path.startswith("/items/"):
            self.reply(404, b'{"error":"not_found"}')
            return
        if not authorized(self):
            self.reply(401, b'{"error":"unauthorized"}')
            return
        sku = self.path[len("/items/"):]
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
        with connect() as connection:
            updated = connection.execute(
                "UPDATE items SET name = ?, qty = ? WHERE sku = ?", (name, quantity, sku)
            ).rowcount
        if not updated:
            self.reply(404, b'{"error":"not_found"}')
            return
        self.reply(200, item_body(sku, name, quantity))

    def do_DELETE(self):  # noqa: N802
        if not self.path.startswith("/items/"):
            self.reply(404, b'{"error":"not_found"}')
            return
        if not authorized(self):
            self.reply(401, b'{"error":"unauthorized"}')
            return
        sku = self.path[len("/items/"):]
        with connect() as connection:
            deleted = connection.execute("DELETE FROM items WHERE sku = ?", (sku,)).rowcount
        if not deleted:
            self.reply(404, b'{"error":"not_found"}')
            return
        self.reply(204)

    def log_message(self, format, *args):  # noqa: A002
        pass


def main():
    port = int(os.environ.get("ATO_ENDPOINT_APP_HTTP_PORT", "8000"))
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()


if __name__ == "__main__":
    main()
