import hashlib
import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# The Model Set and input Assets are delivered by the Runner; the workload only
# reads them. Whatever it leaves in ATO_OUTPUT_DIR is saved when the Run stops.
MODELS = os.environ.get("ATO_INPUT_PATH_MODELS", "")
ASSETS = os.environ.get("ATO_INPUT_ASSETS_DIR", "")
OUTPUT = os.environ.get("ATO_OUTPUT_DIR", "")


def digest(path):
    with open(path, "rb") as handle:
        return hashlib.sha256(handle.read()).hexdigest()


def report():
    out = {"models": {}, "assets": {}, "model_write_denied": None}
    for rel in ("probe/a.bin", "probe/b.txt"):
        path = os.path.join(MODELS, rel)
        try:
            out["models"][rel] = {"bytes": os.path.getsize(path), "sha256": digest(path)}
        except OSError as error:
            out["models"][rel] = {"error": str(error)}
    if ASSETS and os.path.isdir(ASSETS):
        for asset_id in sorted(os.listdir(ASSETS)):
            for name in sorted(os.listdir(os.path.join(ASSETS, asset_id))):
                path = os.path.join(ASSETS, asset_id, name)
                out["assets"][f"{asset_id}/{name}"] = {"bytes": os.path.getsize(path), "sha256": digest(path)}
    try:
        open(os.path.join(MODELS, "probe/write-test"), "w").write("x")
        out["model_write_denied"] = False
    except OSError:
        out["model_write_denied"] = True
    return out


RESULT = report()
if OUTPUT:
    with open(os.path.join(OUTPUT, "result.json"), "w") as handle:
        json.dump(RESULT, handle, sort_keys=True)


class H(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/":
            body = b"model-set-ok\n"
        elif self.path == "/report":
            body = json.dumps(RESULT, sort_keys=True).encode()
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
