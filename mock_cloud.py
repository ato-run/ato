from http.server import BaseHTTPRequestHandler, HTTPServer
import json

class MockHandler(BaseHTTPRequestHandler):
    def do_POST(self):
        if self.path == '/deploy':
            content_length = int(self.headers['Content-Length'])
            post_data = self.rfile.read(content_length)
            print(f"Received POST request to {self.path}")
            print(f"Headers: {self.headers}")
            print(f"Body: {post_data.decode('utf-8')}")
            
            response = {
                "job_id": "job-12345",
                "status": "submitted",
                "endpoint": "http://cloud-instance-1"
            }
            
            self.send_response(200)
            self.send_header('Content-type', 'application/json')
            self.end_headers()
            self.wfile.write(json.dumps(response).encode('utf-8'))
        else:
            self.send_response(404)
            self.end_headers()

def run(server_class=HTTPServer, handler_class=MockHandler, port=8000):
    server_address = ('', port)
    httpd = server_class(server_address, handler_class)
    print(f"Starting mock server on port {port}...")
    httpd.serve_forever()

if __name__ == "__main__":
    run()
