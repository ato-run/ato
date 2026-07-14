#!/usr/bin/env python3
"""Build-only readiness endpoint tied to the expected GUI process/window."""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
import subprocess


APP_PID = int(os.environ["ATO_PIXEL_APP_PID"])
WINDOW_ID = os.environ["ATO_PIXEL_WINDOW_ID"]


def gui_is_ready() -> bool:
    try:
        os.kill(APP_PID, 0)
        wm_class = subprocess.run(
            ["xprop", "-display", os.environ["DISPLAY"], "-id", WINDOW_ID, "WM_CLASS"],
            check=True,
            capture_output=True,
            text=True,
            timeout=1,
        ).stdout
        window = subprocess.run(
            ["xwininfo", "-display", os.environ["DISPLAY"], "-id", WINDOW_ID],
            check=True,
            capture_output=True,
            text=True,
            timeout=1,
        ).stdout
        return "AtoPixelFixture" in wm_class and "Map State: IsViewable" in window
    except (OSError, subprocess.SubprocessError):
        return False


class Handler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        ready = self.path == "/health" and gui_is_ready()
        body = b"ready\n" if ready else b"not ready\n"
        self.send_response(200 if ready else 503)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format: str, *args: object) -> None:
        return


ThreadingHTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
