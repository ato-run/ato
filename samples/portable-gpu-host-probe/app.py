import os
import subprocess
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def gpu_report():
    # Diagnostic only; the Contract observes "/". What the Runner measured is
    # the admission evidence, not this page.
    try:
        out = subprocess.run(
            ["nvidia-smi", "--query-gpu=name,memory.total", "--format=csv,noheader"],
            capture_output=True,
            timeout=10,
            check=False,
        )
        return out.stdout or b"no output\n"
    except (OSError, subprocess.SubprocessError) as error:
        return f"unavailable: {error}\n".encode()


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/":
            body = b"gpu-host-ok\n"
        elif self.path == "/gpu":
            body = gpu_report()
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, _format, *_args):
        pass


port = int(os.environ.get("ATO_ENDPOINT_APP_HTTP_PORT", "8000"))
host = os.environ.get("APP_LISTEN_HOST", "127.0.0.1")
ThreadingHTTPServer((host, port), Handler).serve_forever()
