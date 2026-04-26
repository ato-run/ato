#!/usr/bin/env python3
"""
Simple TODO App - Capsule Entry Point

This is a minimal server that demonstrates running a Capsule-based application.
In a real deployment, this would serve the built React application or provide
a backend API for the TODO application.
"""

import json
import os
import sys
from http.server import HTTPServer, SimpleHTTPRequestHandler
from pathlib import Path

class TodoHandler(SimpleHTTPRequestHandler):
    def do_GET(self):
        """Handle GET requests"""
        if self.path in ("/health", "/api/health"):
            self.send_response(200)
            self.send_header("Content-type", "application/json")
            self.end_headers()
            response = {
                "status": "ok",
                "service": "simple-todo",
                "version": "0.1.0"
            }
            self.wfile.write(json.dumps(response).encode())
        else:
            # Serve static files or return 404
            self.send_response(404)
            self.send_header("Content-type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps({"error": "Not found"}).encode())

    def do_POST(self):
        """Handle POST requests for TODO operations"""
        if self.path == "/api/todos":
            content_length = int(self.headers.get('Content-Length', 0))
            body = self.rfile.read(content_length)
            
            try:
                data = json.loads(body)
                self.send_response(201)
                self.send_header("Content-type", "application/json")
                self.end_headers()
                response = {
                    "id": str(id(data)),
                    "title": data.get("title", ""),
                    "completed": False
                }
                self.wfile.write(json.dumps(response).encode())
            except json.JSONDecodeError:
                self.send_response(400)
                self.send_header("Content-type", "application/json")
                self.end_headers()
                self.wfile.write(json.dumps({"error": "Invalid JSON"}).encode())

    def log_message(self, format, *args):
        """Log server messages"""
        print(f"[{self.client_address[0]}] {format % args}")

def main():
    """Start the TODO application server"""
    print("🚀 Simple TODO App - Capsule Runtime")
    print("=" * 50)
    
    # Get the app directory (use temp dir if /app is not writable)
    app_dir = os.environ.get("APP_DIR", "/app")
    if not os.path.exists(app_dir) or not os.access(os.path.dirname(app_dir) or "/", os.W_OK):
        # Fallback to temp directory or current working directory
        import tempfile
        app_dir = os.environ.get("TMPDIR", tempfile.gettempdir())
    
    try:
        if not os.path.exists(app_dir):
            os.makedirs(app_dir, exist_ok=True)
        os.chdir(app_dir)
    except OSError:
        # Stay in current directory if chdir fails
        app_dir = os.getcwd()
    
    # Server configuration
    host = os.environ.get("CAPSULE_HOST", "0.0.0.0")
    port = int(os.environ.get("CAPSULE_PORT", "8000"))
    
    print(f"📦 Service: simple-todo")
    print(f"� App directory: {app_dir}")
    
    # Socket Activation (Phase 2): Check for inherited socket from parent process
    # Systemd-compatible: LISTEN_FDS environment variable indicates socket activation
    listen_fds = os.environ.get("LISTEN_FDS")
    
    if listen_fds:
        # Socket Activation mode: Use inherited file descriptor
        # SD_LISTEN_FDS_START = 3 (first FD after stdin/stdout/stderr)
        import socket
        fd = 3
        print(f"🔌 Socket Activation: Using inherited FD {fd}")
        print(f"   LISTEN_FDS={listen_fds}, LISTEN_PID={os.environ.get('LISTEN_PID', 'not set')}")
        
        try:
            # Create socket from file descriptor
            # The parent process already bound and is listening on this socket
            sock = socket.fromfd(fd, socket.AF_INET, socket.SOCK_STREAM)
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            
            # Get the actual bound address for logging
            try:
                bound_addr = sock.getsockname()
                print(f"🔗 Socket bound to {bound_addr[0]}:{bound_addr[1]}")
            except Exception:
                print(f"🔗 Socket activated (address lookup failed)")
            
            # Create HTTPServer using the inherited socket
            httpd = HTTPServer(("", 0), TodoHandler, bind_and_activate=False)
            httpd.socket = sock
            # Mark server as already activated
            httpd.server_address = sock.getsockname()
            
            print("=" * 50)
            print("✅ Socket Activation: Ready to accept connections")
            
            try:
                httpd.serve_forever()
            except KeyboardInterrupt:
                print("\n⏹️  Shutting down...")
                httpd.server_close()
                sys.exit(0)
        except Exception as e:
            print(f"❌ Socket Activation failed: {e}")
            print("   Falling back to traditional bind...")
            # Fall through to traditional binding
            listen_fds = None
    
    if not listen_fds:
        # Traditional mode: Bind socket ourselves
        print(f"🔗 Traditional mode: Binding to {host}:{port}")
        print("=" * 50)
        
        server_address = (host, port)
        httpd = HTTPServer(server_address, TodoHandler)
        
        try:
            httpd.serve_forever()
        except KeyboardInterrupt:
            print("\n⏹️  Shutting down...")
            httpd.server_close()
            sys.exit(0)

if __name__ == "__main__":
    main()
