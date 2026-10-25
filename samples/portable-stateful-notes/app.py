"""A tiny write-capable HTTP application backed by a declared SQLite state slot."""

import json
import os
import sqlite3
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


DB_PATH = os.environ.get("APP_DB_PATH", "./notes.sqlite")


def connect() -> sqlite3.Connection:
    os.makedirs(os.path.dirname(DB_PATH) or ".", exist_ok=True)
    connection = sqlite3.connect(DB_PATH)
    connection.row_factory = sqlite3.Row
    return connection


def initialize() -> None:
    with connect() as connection:
        connection.execute(
            "CREATE TABLE IF NOT EXISTS notes ("
            "id INTEGER PRIMARY KEY AUTOINCREMENT, body TEXT NOT NULL)"
        )


class NotesHandler(BaseHTTPRequestHandler):
    server_version = "PortableNotes/1"

    def send_bytes(self, status: int, body: bytes, content_type: str) -> None:
        self.send_response(status)
        self.send_header("content-type", content_type)
        self.send_header("content-length", str(len(body)))
        self.send_header("cache-control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def send_json(self, status: int, value: object) -> None:
        body = json.dumps(value, separators=(",", ":")).encode("utf-8")
        self.send_bytes(status, body, "application/json")

    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler ABI
        if self.path == "/health":
            with connect() as connection:
                connection.execute("SELECT 1 FROM notes LIMIT 1")
            self.send_bytes(HTTPStatus.OK, b'{"ok":true}', "application/json")
            return
        if self.path == "/api/notes":
            with connect() as connection:
                rows = connection.execute(
                    "SELECT id, body FROM notes ORDER BY id"
                ).fetchall()
            self.send_json(HTTPStatus.OK, {"notes": [dict(row) for row in rows]})
            return
        if self.path == "/":
            self.send_bytes(
                HTTPStatus.OK,
                INDEX_HTML.encode("utf-8"),
                "text/html; charset=utf-8",
            )
            return
        self.send_json(HTTPStatus.NOT_FOUND, {"error": "not_found"})

    def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler ABI
        if self.path != "/api/notes":
            self.send_json(HTTPStatus.NOT_FOUND, {"error": "not_found"})
            return
        try:
            length = int(self.headers.get("content-length", "0"))
        except ValueError:
            length = -1
        if not 0 <= length <= 64 * 1024:
            self.send_json(HTTPStatus.BAD_REQUEST, {"error": "invalid_length"})
            return
        try:
            value = json.loads(self.rfile.read(length))
            raw_body = value["body"]
            if not isinstance(raw_body, str):
                raise ValueError
            body = raw_body.strip()
            if not 1 <= len(body) <= 1000:
                raise ValueError
        except (json.JSONDecodeError, KeyError, TypeError, ValueError):
            self.send_json(HTTPStatus.BAD_REQUEST, {"error": "invalid_note"})
            return
        with connect() as connection:
            cursor = connection.execute("INSERT INTO notes (body) VALUES (?)", (body,))
            note = connection.execute(
                "SELECT id, body FROM notes WHERE id = ?", (cursor.lastrowid,)
            ).fetchone()
        self.send_json(HTTPStatus.CREATED, {"note": dict(note)})

    def log_message(self, format: str, *args: object) -> None:
        print(f"notes-http: {format % args}", flush=True)


INDEX_HTML = """<!doctype html>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Portable Notes</title>
<style>
  body { font: 16px/1.5 system-ui,sans-serif; margin: 2rem auto; max-width: 38rem; padding: 0 1rem; }
  form { display: flex; gap: .5rem; }
  input { flex: 1; padding: .65rem; }
  button { padding: .65rem 1rem; }
  li { margin: .6rem 0; }
</style>
<h1>Portable Notes</h1>
<form id="note-form"><input id="note" maxlength="1000" placeholder="Write a note" autofocus><button>Add</button></form>
<ol id="notes"></ol>
<script>
  const list = document.querySelector('#notes');
  const field = document.querySelector('#note');
  async function refresh() {
    const response = await fetch('/api/notes', { cache: 'no-store' });
    const { notes } = await response.json();
    list.replaceChildren(...notes.map((note) => {
      const item = document.createElement('li');
      item.textContent = note.body;
      return item;
    }));
  }
  document.querySelector('#note-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    if (!field.value.trim()) return;
    await fetch('/api/notes', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ body: field.value })
    });
    field.value = '';
    await refresh();
  });
  refresh();
</script>
"""


if __name__ == "__main__":
    initialize()
    port = int(os.environ.get("ATO_ENDPOINT_APP_HTTP_PORT", "8000"))
    if not 1 <= port <= 65535:
        raise ValueError("invalid HTTP port")
    ThreadingHTTPServer(("0.0.0.0", port), NotesHandler).serve_forever()
