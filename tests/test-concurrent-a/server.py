"""
Test F: Concurrent App A
Another dynamic server to test concurrent deployments.
"""
import http.server
import socketserver
import os
import random

PORT = int(os.environ.get('PORT', 8000))
APP_ID = f"App-A-{random.randint(1000, 9999)}"

class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-type', 'text/html')
        self.end_headers()
        html = f"""
<!DOCTYPE html>
<html>
<head>
    <title>Test F - Concurrent App A</title>
    <style>
        body {{
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            background: linear-gradient(135deg, #11998e 0%, #38ef7d 100%);
            color: white;
            display: flex;
            justify-content: center;
            align-items: center;
            min-height: 100vh;
            margin: 0;
        }}
        .card {{
            background: rgba(255,255,255,0.15);
            backdrop-filter: blur(10px);
            border-radius: 20px;
            padding: 40px;
            text-align: center;
            box-shadow: 0 8px 32px rgba(0,0,0,0.3);
        }}
        h1 {{ margin: 0 0 10px; font-size: 2em; }}
        .port {{ 
            font-size: 2.5em; 
            font-weight: bold;
            color: #fff;
            margin: 15px 0;
            padding: 10px 20px;
            background: rgba(0,0,0,0.2);
            border-radius: 10px;
        }}
        .app-id {{
            font-size: 0.9em;
            opacity: 0.7;
            margin-top: 15px;
        }}
    </style>
</head>
<body>
    <div class="card">
        <h1>🟢 Concurrent App A</h1>
        <p>Running on port:</p>
        <div class="port">{PORT}</div>
        <div class="app-id">Instance: {APP_ID}</div>
    </div>
</body>
</html>
"""
        self.wfile.write(html.encode())

print(f"[{APP_ID}] Starting server on port {PORT}...")
with socketserver.TCPServer(("", PORT), Handler) as httpd:
    print(f"[{APP_ID}] Server running at http://127.0.0.1:{PORT}")
    httpd.serve_forever()
