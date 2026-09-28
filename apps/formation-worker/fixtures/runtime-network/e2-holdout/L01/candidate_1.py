import os
from http.server import HTTPServer, SimpleHTTPRequestHandler


os.chdir(os.path.dirname(os.path.abspath(__file__)))
HTTPServer(("127.0.0.1", int(os.environ["ATO_ENDPOINT_APP_HTTP_PORT"])), SimpleHTTPRequestHandler).serve_forever()
