"""Fixed fixture launcher for unmodified CPython's OSS HTTP server.
The logical port is bound by Ato's existing process adapter, not the producer.
"""
import os
from pathlib import Path
import oss_http

os.chdir(Path(__file__).resolve().parent)
oss_http.test(
    HandlerClass=oss_http.SimpleHTTPRequestHandler,
    ServerClass=oss_http.ThreadingHTTPServer,
    port=int(os.environ["ATO_ENDPOINT_APP_HTTP_PORT"]),
    bind="127.0.0.1",
)
