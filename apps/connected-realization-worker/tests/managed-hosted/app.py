"""CPU acceptance workload; all external operations use the declared Binding."""
import http.server
import json
import os
import urllib.request

mode = os.environ["MODE"]
dimensions = 0
if mode == "backend":
    request = urllib.request.Request(
        os.environ["OPENAI_BASE_URL"] + "/embeddings",
        data=json.dumps({"input": "one bounded document", "encoding_format": "float"}).encode(),
        headers={"Authorization": "Bearer " + os.environ["OPENAI_API_KEY"], "Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=8) as response:
        dimensions = len(json.load(response)["data"][0]["embedding"])
    if dimensions != 1536:
        raise RuntimeError("unexpected embedding dimensions")
    dimensions = int(os.environ["REPORTED_DIMENSIONS"])


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if mode == "backend":
            body = ("dimensions=" + str(dimensions) + "\n").encode()
        else:
            with urllib.request.urlopen("http://backend:8080/health", timeout=3) as response:
                response.read()
            body = b"application ready\n"
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


http.server.ThreadingHTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
