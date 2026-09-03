"""Acceptance fixture: a notes service on SQLite, standard library only.

Deliberately boring. It exists to make one claim checkable end to end — that a
ComputeInstance keeps its state across Runs that share no process, no PID and
no workspace — so anything that could fail for its own reasons is left out.

No pip, no FastAPI, no uvicorn: P3.0 established that installed dependencies
cannot yet be carried as a Formation artifact, so a fixture needing them would
be testing a lane that does not exist.

## The two paths it proves

    DATABASE_PATH=/data/app.sqlite   -> the state mount, read-write
    /app                             -> the workspace, read-only

`/data` is a real bind mount of the attachment, which is why this reads
`DATABASE_PATH` rather than a Runner-specific variable. `/app` being read-only
is why byte-code writing must be off (`python3 -B`, or
PYTHONDONTWRITEBYTECODE=1) — otherwise CPython tries to create `__pycache__`
beside this file and the process dies on startup.

## The port

    ATO_ENDPOINT_HTTP_PORT=<port the Runner actually allocated>

NOT a fixed 8000. The process realization shares the host network namespace, so
there is no guest->host NAT to translate a fixed guest port; the port the
workload binds is a real host port and only the Runner knows which one is free.
"""

import json
import os
import sqlite3
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

DATABASE_PATH = os.environ.get("DATABASE_PATH", "/data/app.sqlite")


def connect() -> sqlite3.Connection:
    """One connection per request, opened and closed around it.

    A long-lived connection would keep the database file open across the
    Runner's SIGTERM, and the state pack that follows could see a
    partially-flushed file. Opening per request means a committed write is on
    disk before the response is sent.
    """
    connection = sqlite3.connect(DATABASE_PATH, timeout=10)
    # DELETE rather than WAL: WAL leaves -wal and -shm sidecars whose contents
    # matter, and the acceptance is about the database surviving, not about
    # exercising SQLite's journal modes.
    connection.execute("PRAGMA journal_mode=DELETE")
    connection.execute("CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY AUTOINCREMENT, body TEXT NOT NULL)")
    connection.commit()
    return connection


class Handler(BaseHTTPRequestHandler):
    # Quiet: the Runner captures stdio, and a request log per health poll would
    # bury anything worth reading.
    def log_message(self, *_args):
        return

    def _respond(self, status: int, payload) -> None:
        body = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/health":
            # Readiness means the DATABASE is reachable, not merely that the
            # process is up. A workload that reports ready before its state
            # mount works would have the Runner stop it and commit nothing.
            try:
                connection = connect()
                connection.execute("SELECT 1").fetchone()
                connection.close()
            except Exception as error:  # noqa: BLE001 - reported, not swallowed
                self._respond(503, {"ok": False, "error": str(error)})
                return
            self._respond(200, {"ok": True})
            return

        if self.path == "/notes":
            connection = connect()
            rows = connection.execute("SELECT id, body FROM notes ORDER BY id").fetchall()
            connection.close()
            self._respond(200, [{"id": row[0], "body": row[1]} for row in rows])
            return

        self._respond(404, {"error": "not_found"})

    def do_POST(self):
        if self.path != "/notes":
            self._respond(404, {"error": "not_found"})
            return

        length = int(self.headers.get("content-length") or 0)
        try:
            payload = json.loads(self.rfile.read(length) or b"{}")
            body = payload["body"]
            if not isinstance(body, str) or not body:
                raise ValueError("body must be a non-empty string")
        except Exception as error:  # noqa: BLE001
            self._respond(400, {"error": str(error)})
            return

        connection = connect()
        connection.execute("INSERT INTO notes (body) VALUES (?)", (body,))
        # Committed and closed BEFORE the response. The acceptance treats a 201
        # as "this is on disk", and stops the Run shortly after.
        connection.commit()
        connection.close()
        self._respond(201, {"ok": True})


def main() -> None:
    port = int(os.environ["ATO_ENDPOINT_HTTP_PORT"])
    # Bind LAST. Everything a request needs is ready by the time the socket
    # accepts, so a successful connection is a real readiness signal rather
    # than a race the Runner has to tolerate.
    server = ThreadingHTTPServer(("0.0.0.0", port), Handler)
    server.serve_forever()


if __name__ == "__main__":
    main()
