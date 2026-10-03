import hashlib
import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# The Model Set is delivered by the Runner; the workload only reads it.
MODELS = os.environ.get("ATO_INPUT_PATH_MODELS", "")


def model_report():
    out = {"root": MODELS, "files": {}}
    for rel in ("probe/a.bin", "probe/b.txt"):
        path = os.path.join(MODELS, rel)
        try:
            data = open(path, "rb").read()
            out["files"][rel] = {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
        except OSError as e:
            out["files"][rel] = {"error": str(e)}
    try:
        open(os.path.join(MODELS, "probe/write-test"), "w").write("x")
        out["writable"] = True
    except OSError:
        out["writable"] = False
    return out


class H(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/":
            body = b"model-set-ok\n"
        elif self.path == "/models":
            body = json.dumps(model_report()).encode()
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


ThreadingHTTPServer((os.environ.get("APP_LISTEN_HOST", "127.0.0.1"), int(os.environ.get("ATO_ENDPOINT_APP_HTTP_PORT", "8000"))), H).serve_forever()
