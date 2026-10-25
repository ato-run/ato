"""Backend of the persistent volume proof.

PUT /api/value stores the request body; it answers 200 only after SQLite has
committed with synchronous=FULL, i.e. after the write is durable on /data.
GET /api/value returns the stored value (404 when there is none).
GET /api/mounts reports whether /data is a mount point in THIS container.
Standard library only.
"""

import json
import os
import signal
import sqlite3
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

DATA = os.environ.get("ATO_STATE_PATH_DATA", "/data")
DATABASE = os.path.join(DATA, "proof.sqlite")
MAX_VALUE = 4096


def connect():
    connection = sqlite3.connect(DATABASE)
    connection.execute("PRAGMA journal_mode=WAL")
    connection.execute("PRAGMA synchronous=FULL")
    connection.execute(
        "CREATE TABLE IF NOT EXISTS proof (id INTEGER PRIMARY KEY CHECK (id = 1), value TEXT)"
    )
    return connection


def mounted(path):
    with open("/proc/self/mounts", encoding="utf-8") as mounts:
        return any(line.split()[1] == path for line in mounts if len(line.split()) > 1)


class Handler(BaseHTTPRequestHandler):
    def reply(self, status, body, content_type="text/plain; charset=utf-8"):
        payload = body.encode("utf-8")
        self.send_response(status)
        self.send_header("content-type", content_type)
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        if self.path == "/api/healthz":
            connect().close()
            return self.reply(200, "ok\n")
        if self.path == "/api/value":
            with connect() as connection:
                row = connection.execute("SELECT value FROM proof WHERE id = 1").fetchone()
            return self.reply(200, row[0]) if row else self.reply(404, "no value\n")
        if self.path == "/api/mounts":
            return self.reply(
                200,
                json.dumps({"data_mounted": mounted(DATA)}),
                "application/json",
            )
        return self.reply(404, "not found\n")

    def do_PUT(self):
        if self.path != "/api/value":
            return self.reply(404, "not found\n")
        length = int(self.headers.get("content-length") or 0)
        if length <= 0 or length > MAX_VALUE:
            return self.reply(400, "value must be 1..4096 bytes\n")
        value = self.rfile.read(length).decode("utf-8")
        connection = connect()
        try:
            with connection:
                connection.execute(
                    "INSERT INTO proof (id, value) VALUES (1, ?) "
                    "ON CONFLICT(id) DO UPDATE SET value = excluded.value",
                    (value,),
                )
        finally:
            connection.close()
        return self.reply(200, "committed\n")

    def log_message(self, format, *args):
        pass


def stop(signum, frame):
    # As PID 1 the default SIGTERM action is ignored; exit on the stop signal
    # so a normal stop is graceful. Committed writes are already durable.
    sys.exit(0)


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, stop)
    ThreadingHTTPServer(("0.0.0.0", 8081), Handler).serve_forever()
