import http.server
import os
import socket
import socketserver

HOST = os.environ.get("ATO_SERVICE_DB_HOST", "127.0.0.1")
PORT = int(os.environ["ATO_SERVICE_DB_PORT"])
LISTEN_PORT = int(os.environ.get("PORT", "18081"))
KEY = b"ato:e2e"
VALUE = b"native-loopback-ok"


def send_redis_command(*parts: bytes) -> bytes:
    payload = [f"*{len(parts)}\r\n".encode("utf-8")]
    for part in parts:
        payload.append(f"${len(part)}\r\n".encode("utf-8"))
        payload.append(part + b"\r\n")
    with socket.create_connection((HOST, PORT), timeout=5) as conn:
        conn.sendall(b"".join(payload))
        conn.shutdown(socket.SHUT_WR)
        return conn.recv(4096)


pong = send_redis_command(b"PING")
if b"PONG" not in pong:
    raise SystemExit(f"Redis PING failed: {pong!r}")

set_result = send_redis_command(b"SET", KEY, VALUE)
if b"OK" not in set_result:
    raise SystemExit(f"Redis SET failed: {set_result!r}")

get_result = send_redis_command(b"GET", KEY)
if VALUE not in get_result:
    raise SystemExit(f"Redis GET failed: {get_result!r}")

BODY = f"db=ok host={HOST} port={PORT} value={VALUE.decode('utf-8')}".encode("utf-8")


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(BODY)))
        self.end_headers()
        self.wfile.write(BODY)

    def log_message(self, format, *args):
        return


class ReusableTCPServer(socketserver.TCPServer):
    allow_reuse_address = True


with ReusableTCPServer(("127.0.0.1", LISTEN_PORT), Handler) as httpd:
    httpd.serve_forever()
