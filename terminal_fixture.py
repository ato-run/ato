#!/usr/bin/env python3
"""Deterministic Terminal Surface v1 fixture; stdlib only."""

import http.server
import os
import signal
import sys
import threading


class Health(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path not in ("/", "/health"):
            self.send_response(404)
            self.end_headers()
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.end_headers()
        self.wfile.write(b"ready\n")

    def log_message(self, _format, *_args):
        pass


def size():
    terminal = os.get_terminal_size(sys.stdout.fileno())
    return terminal.columns, terminal.lines


def write(value):
    sys.stdout.write(value)
    sys.stdout.flush()


def on_resize(_signal, _frame):
    columns, rows = size()
    write(f"\r\nresize:{columns}x{rows}\r\nfixture> ")


def restore_and_exit(code):
    write("\x1b[?25h\x1b[?1049l")
    raise SystemExit(code)


def on_interrupt(_signal, _frame):
    write("\r\nctrl-c\r\n")
    restore_and_exit(0)


threading.Thread(
    target=http.server.ThreadingHTTPServer(("0.0.0.0", 18080), Health).serve_forever,
    daemon=True,
).start()
signal.signal(signal.SIGWINCH, on_resize)
signal.signal(signal.SIGINT, on_interrupt)

columns, rows = size()
write("\x1b[?1049h\x1b[2J\x1b[H")
write("\x1b[1;36mAto Terminal Surface v1\x1b[0m\r\n")
write(f"size:{columns}x{rows}\r\n")
write("type text and press Enter; type exit to finish; Ctrl+C is a clean exit\r\n")

try:
    while True:
        write("fixture> ")
        line = sys.stdin.readline()
        if not line:
            restore_and_exit(0)
        value = line.rstrip("\r\n")
        if value == "exit":
            write("bye\r\n")
            restore_and_exit(0)
        write(f"echo:{value}\r\n")
except KeyboardInterrupt:
    on_interrupt(signal.SIGINT, None)
