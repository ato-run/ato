"""Source B: socketserver.TCPServer with raw HTTP request parsing.

This is deliberately a different stdlib code path from source A (no
http.server request handler, no send_response/send_header), while the response
bytes are the same identity the frozen Contract pins:

    {"service":"transformation-v0","status":"ok"}\n
"""

import os
import socketserver

BODY = b'{"service":"transformation-v0","status":"ok"}\n'


def render(status: bytes, body: bytes) -> bytes:
    return (
        b"HTTP/1.1 "
        + status
        + b"\r\n"
        + b"Content-Type: application/json\r\n"
        + b"Content-Length: "
        + str(len(body)).encode("ascii")
        + b"\r\n"
        + b"Connection: close\r\n"
        + b"\r\n"
        + body
    )


class Handler(socketserver.BaseRequestHandler):
    def handle(self) -> None:  # pragma: no cover - exercised by the E2E
        request = b""
        while b"\r\n" not in request:
            chunk = self.request.recv(4096)
            if not chunk:
                return
            request += chunk
        parts = request.split(b"\r\n", 1)[0].split(b" ")
        ok = len(parts) == 3 and parts[0] == b"GET" and parts[1] == b"/"
        if ok:
            self.request.sendall(render(b"200 OK", BODY))
        else:
            self.request.sendall(render(b"404 Not Found", b""))


class Server(socketserver.TCPServer):
    allow_reuse_address = True


def main() -> None:
    port = int(os.environ.get("ATO_ENDPOINT_APP_HTTP_PORT", "8000"))
    with Server(("127.0.0.1", port), Handler) as server:
        server.serve_forever()


if __name__ == "__main__":
    main()
