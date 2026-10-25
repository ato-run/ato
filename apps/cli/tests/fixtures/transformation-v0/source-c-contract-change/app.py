"""Source C: source A's implementation with a different authored Contract.

The server itself is A's; the manifest's observation differs (status 201), so
the candidate derives a different ContractRef and `ato reform` refuses it
before anything is built or executed.
"""

import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

BODY = b'{"service":"transformation-v0","status":"ok"}\n'


class Handler(BaseHTTPRequestHandler):
    server_version = "transformation-v0-c/1"

    def do_GET(self):  # noqa: N802 - BaseHTTPRequestHandler's API
        if self.path != "/":
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(BODY)))
        self.end_headers()
        self.wfile.write(BODY)

    def log_message(self, format, *args):  # noqa: A002 - stdlib signature
        pass


def main() -> None:
    port = int(os.environ.get("ATO_ENDPOINT_APP_HTTP_PORT", "8000"))
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()


if __name__ == "__main__":
    main()
