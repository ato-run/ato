from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os

HOST = os.environ.get("DESKY_SESSION_HOST", "127.0.0.1")
PORT = int(os.environ.get("DESKY_SESSION_PORT", "43123"))
ADAPTER = os.environ.get("DESKY_SESSION_ADAPTER", "tauri")
SESSION_ID = os.environ.get("DESKY_SESSION_ID", "desky-session")
GUEST_MODE = os.environ.get("ATO_GUEST_MODE")


class Handler(BaseHTTPRequestHandler):
    def _send_json(self, status, payload):
        body = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path != "/health":
            self._send_json(404, {"ok": False, "error": "not_found"})
            return
        self._send_json(
            200,
            {
                "ok": True,
                "adapter": ADAPTER,
                "session_id": SESSION_ID,
                "guest_mode": GUEST_MODE,
            },
        )

    def do_POST(self):
        if self.path != "/rpc":
            self._send_json(404, {"ok": False, "error": "not_found"})
            return
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length) if length > 0 else b"{}"
        request = json.loads(raw.decode("utf-8"))
        params = request.get("params") or {}
        payload = params.get("payload") or {}
        message = payload.get("message", "")
        command = params.get("command", "unknown")

        if command == "check_env":
            self._send_json(
                200,
                {
                    "jsonrpc": "2.0",
                    "id": request.get("id"),
                    "result": {
                        "ok": True,
                        "adapter": ADAPTER,
                        "session_id": SESSION_ID,
                        "ato_guest_mode": GUEST_MODE,
                    },
                },
            )
            return

        self._send_json(
            200,
            {
                "jsonrpc": "2.0",
                "id": request.get("id"),
                "result": {
                    "ok": True,
                    "adapter": ADAPTER,
                    "session_id": SESSION_ID,
                    "command": command,
                    "echo": message,
                },
            },
        )

    def log_message(self, fmt, *args):
        return


if __name__ == "__main__":
    server = ThreadingHTTPServer((HOST, PORT), Handler)
    server.serve_forever()
