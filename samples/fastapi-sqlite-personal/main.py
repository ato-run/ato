"""A note keeper, small enough to read in one sitting.

It exists to answer one question in a browser: does data written here survive
the app going to sleep and waking up again? So every route is about notes and
nothing here knows what a Run, a slot or a revision is — the database lives at
`APP_DB_PATH`, under the mount `capsule.toml` declared, and that is the whole
contract.
"""

import os
import sqlite3
from contextlib import closing

from fastapi import FastAPI
from fastapi.responses import HTMLResponse, JSONResponse
from pydantic import BaseModel

# Declared in `capsule.toml`. The default is for running this on a laptop; on
# Ato the environment carries the mounted path and this never guesses.
DB_PATH = os.environ.get("APP_DB_PATH", "./app.sqlite")

app = FastAPI()


def connect() -> sqlite3.Connection:
    os.makedirs(os.path.dirname(DB_PATH) or ".", exist_ok=True)
    connection = sqlite3.connect(DB_PATH)
    connection.row_factory = sqlite3.Row
    return connection


def initialize() -> None:
    with closing(connect()) as connection:
        connection.execute(
            """CREATE TABLE IF NOT EXISTS notes (
                 id      INTEGER PRIMARY KEY AUTOINCREMENT,
                 body    TEXT NOT NULL,
                 written TEXT NOT NULL DEFAULT (datetime('now'))
               )"""
        )
        connection.commit()


initialize()


class NewNote(BaseModel):
    body: str


@app.get("/health")
def health() -> JSONResponse:
    # What `readiness_path` names. It touches the database on purpose: a
    # process that is listening but cannot reach its own data is not ready,
    # and reporting otherwise would hand somebody a broken app and call it up.
    with closing(connect()) as connection:
        connection.execute("SELECT 1 FROM notes LIMIT 1")
    return JSONResponse({"ok": True})


@app.get("/api/notes")
def list_notes() -> JSONResponse:
    with closing(connect()) as connection:
        rows = connection.execute(
            "SELECT id, body, written FROM notes ORDER BY id DESC"
        ).fetchall()
    return JSONResponse({"notes": [dict(row) for row in rows]})


@app.post("/api/notes")
def add_note(note: NewNote) -> JSONResponse:
    body = note.body.strip()
    if not body:
        return JSONResponse({"error": "a note needs something in it"}, status_code=400)
    with closing(connect()) as connection:
        cursor = connection.execute("INSERT INTO notes (body) VALUES (?)", (body,))
        connection.commit()
        row = connection.execute(
            "SELECT id, body, written FROM notes WHERE id = ?", (cursor.lastrowid,)
        ).fetchone()
    return JSONResponse({"note": dict(row)}, status_code=201)


@app.get("/", response_class=HTMLResponse)
def index() -> HTMLResponse:
    # Served by the app itself rather than as a static file: a person driving
    # the acceptance needs to see the notes come back, and a page that fetches
    # them proves the process answered, not that a cache did.
    return HTMLResponse(
        """<!doctype html>
<meta charset="utf-8">
<title>Notes</title>
<style>
  body { font: 16px/1.5 system-ui, sans-serif; margin: 2rem auto; max-width: 34rem; }
  li { margin: .4rem 0; }
  time { color: #666; font-size: .85em; margin-left: .5rem; }
</style>
<h1>Notes</h1>
<form id="f"><input id="b" placeholder="Write a note" size="34" autofocus>
<button>Add</button></form>
<ul id="list"></ul>
<script>
  const list = document.getElementById("list");
  async function load() {
    const { notes } = await (await fetch("/api/notes")).json();
    list.innerHTML = notes
      .map((n) => `<li>${n.body}<time>${n.written}</time></li>`)
      .join("");
  }
  document.getElementById("f").onsubmit = async (event) => {
    event.preventDefault();
    const field = document.getElementById("b");
    if (!field.value.trim()) return;
    await fetch("/api/notes", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ body: field.value }),
    });
    field.value = "";
    load();
  };
  load();
</script>"""
    )
