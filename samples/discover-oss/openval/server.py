"""Serve the OpenVal static export and its existing Vercel HTTP handlers.

OPENVAL_ROOT points to the pinned upstream checkout. This adapter does not
replace the underwriting engine or the workbook handlers.
"""

from __future__ import annotations

import os
import sys
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit

ROOT = Path(os.environ.get("OPENVAL_ROOT", ".")).resolve()
sys.path.insert(0, str(ROOT / "web"))

from api import cashflow, export_workbook, parse_workbook  # noqa: E402

MAX_BODY_BYTES = 10 * 1024 * 1024
API_ROUTES = {
    "/api/cashflow": cashflow.handler,
    "/api/parse_workbook": parse_workbook.handler,
    "/api/export_workbook": export_workbook.handler,
}


class Handler(
    cashflow.handler,
    parse_workbook.handler,
    export_workbook.handler,
    SimpleHTTPRequestHandler,
):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(ROOT / "web" / "out"), **kwargs)

    def do_GET(self):
        route = API_ROUTES.get(urlsplit(self.path).path)
        if route:
            return route.do_GET(self)
        if urlsplit(self.path).path.startswith("/api/"):
            return self.send_error(404)
        return SimpleHTTPRequestHandler.do_GET(self)

    def do_POST(self):
        route = API_ROUTES.get(urlsplit(self.path).path)
        if not route:
            return self.send_error(404)
        try:
            length = int(self.headers.get("Content-Length", ""))
        except ValueError:
            return self.send_error(411)
        if length < 0 or length > MAX_BODY_BYTES:
            return self.send_error(413)
        return route.do_POST(self)

    def do_OPTIONS(self):
        route = API_ROUTES.get(urlsplit(self.path).path)
        if not route:
            return self.send_error(404)
        return route.do_OPTIONS(self)


if __name__ == "__main__":
    server = ThreadingHTTPServer(
        (
            os.environ.get("HOST", "127.0.0.1"),
            int(os.environ.get("ATO_ENDPOINT_APP_HTTP_PORT", os.environ.get("PORT", "8000"))),
        ),
        Handler,
    )
    server.serve_forever()
