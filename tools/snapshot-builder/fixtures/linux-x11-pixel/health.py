#!/usr/bin/env python3
"""Build-only readiness endpoint tied to the expected GUI process/window."""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
import socket
import subprocess


APP_PID = int(os.environ["ATO_PIXEL_APP_PID"])
WINDOW_ID = os.environ["ATO_PIXEL_WINDOW_ID"]
RFB_PORT = int(os.environ.get("ATO_PIXEL_RFB_PORT", "5900"))


def rfb_is_accepting() -> bool:
    # The snapshot must NOT seal until x11vnc is actually accepting on the RFB
    # port: the sealed frame is a memory resume, so whatever is (not) listening
    # at seal time is (not) listening at restore time. Gating readiness on the
    # window alone let the seal race ahead of the x11vnc bind, and the restored
    # guest then refused the host gateway's RFB connect (connection refused on
    # 5900 → pixel surface never becomes interactive).
    try:
        with socket.create_connection(("127.0.0.1", RFB_PORT), timeout=1):
            return True
    except OSError:
        return False


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
        return (
            "AtoPixelFixture" in wm_class
            and "Map State: IsViewable" in window
            and rfb_is_accepting()
        )
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
