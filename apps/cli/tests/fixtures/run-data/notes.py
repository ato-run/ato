from html import escape
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
import os
import sys
from urllib.parse import parse_qs

notes = Path(os.environ["ATO_STATE_PATH_NOTES"])
note = notes / "note.txt"


class Notes(BaseHTTPRequestHandler):
    def reply(self, status, body=b"", content_type="text/plain; charset=utf-8"):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/healthz":
            key = os.environ.get("ATO_BINDING_ACCESS_KEY")
            self.reply(200 if key else 503)
        elif self.path == "/":
            content = note.read_bytes() if note.exists() else b""
            if "text/html" in self.headers.get("Accept", ""):
                text = escape(content.decode("utf-8", errors="replace"))
                page = (
                    '<!doctype html><html lang="ja"><meta charset="utf-8">'
                    "<title>ローカルメモ</title><h1>ローカルメモ</h1>"
                    '<form method="post"><label for="note">メモ</label><br>'
                    f'<textarea id="note" name="note" rows="8" cols="50">{text}</textarea>'
                    '<br><button type="submit">メモを更新</button></form></html>'
                )
                self.reply(200, page.encode(), "text/html; charset=utf-8")
            else:
                self.reply(200, content)
        else:
            self.reply(404)

    def do_POST(self):
        if self.path != "/":
            self.reply(404)
            return
        if self.headers.get("X-Test-Link") == "1":
            (notes / "link").symlink_to("note.txt")
        body = self.rfile.read(int(self.headers["Content-Length"]))
        if self.headers.get("Content-Type") == "application/x-www-form-urlencoded":
            text = parse_qs(body.decode(), keep_blank_values=True).get("note", [""])[0]
            note.write_text(text, encoding="utf-8")
            self.send_response(303)
            self.send_header("Location", "/")
            self.end_headers()
            return
        note.write_bytes(body)
        self.reply(204)


HTTPServer(("127.0.0.1", int(sys.argv[1])), Notes).serve_forever()
