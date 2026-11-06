"""Proxy of the native process group.

Serves the Surface and reaches the backend only through the Port its D
declared (`backend.http`). The value it reads is the Runtime's loopback shim,
not the backend's real host port.
"""

import errno
import os
import socket
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

BACKEND_PORT = int(os.environ["ATO_ENDPOINT_BACKEND_HTTP_PORT"])


def get_from_backend(path):
    connection = socket.create_connection(("127.0.0.1", BACKEND_PORT), timeout=5)
    try:
        request = f"GET {path} HTTP/1.0\r\nHost: backend\r\n\r\n".encode("ascii")
        connection.sendall(request)
        chunks = []
        while True:
            chunk = connection.recv(4096)
            if not chunk:
                break
            chunks.append(chunk)
    finally:
        connection.close()
    response = b"".join(chunks)
    return response.split(b"\r\n\r\n", 1)[1]


def probe_undeclared_egress():
    # A destination the D did not declare. Only the sandbox's refusal is the
    # expected outcome; a timeout or a successful connect is reported as such.
    try:
        connection = socket.create_connection(("1.1.1.1", 80), timeout=2)
    except OSError as error:
        # The kernel's refusal is EPERM on macOS and EACCES on Linux; both
        # mean "denied". Reporting the platform's errno would make K's
        # observation depend on the OS, so both normalize to one body.
        if error.errno in (errno.EPERM, errno.EACCES):
            return b"denied\n"
        return f"reached:{error.errno}\n".encode("ascii")
    connection.close()
    return b"reached:connected\n"


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        try:
            if self.path == "/healthz":
                body = b"native-proxy-ok\n"
            elif self.path == "/":
                body = get_from_backend("/")
            elif self.path == "/egress":
                body = probe_undeclared_egress()
            else:
                self.send_error(404)
                return
        except OSError as error:
            # The failing leg is not visible in K; say it on stderr so a failed
            # observation has a cause in the Run's own output.
            sys.stderr.write(f"proxy: {self.path}: {error!r}\n")
            sys.stderr.flush()
            self.send_error(502)
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, _format, *_args):
        pass


port = int(os.environ["ATO_ENDPOINT_APP_HTTP_PORT"])
ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
