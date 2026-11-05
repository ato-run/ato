#!/usr/bin/env python3
"""Local prototype: bounded HIBP range relay over a granted SOCKS5 path.

This is not a deployed service. No request path, hash prefix, or credential is
logged. Production use requires an explicit grant and the checks in README.
"""
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import ipaddress
import os
import re
import signal
import socket
import ssl
import struct
import threading
from urllib.parse import urlsplit

HOST = 'api.pwnedpasswords.com'
MAX_BYTES = 2 * 1024 * 1024
TIMEOUT = 10
PREFIX = re.compile(r'/range/[0-9A-F]{5}\Z')
LINE = re.compile(rb'[0-9A-F]{35}:[0-9]{1,12}\Z')


def parse_config(env):
    raw_broker = env.get('ATO_BINDING_BREACH_EGRESS')
    raw_target = env.get('ATO_BINDING_BREACH_TARGET')
    broker = None
    target = None
    if raw_broker:
        url = urlsplit(raw_broker)
        if (url.scheme != 'socks5' or not url.hostname or not url.port or
                url.username or url.password or url.path or url.query or url.fragment):
            raise ValueError('invalid SOCKS5 broker')
        address = ipaddress.IPv4Address(url.hostname)
        private_networks = (ipaddress.ip_network('10.0.0.0/8'),
                            ipaddress.ip_network('172.16.0.0/12'),
                            ipaddress.ip_network('192.168.0.0/16'))
        if not any(address in network for network in private_networks):
            raise ValueError('broker must be a private IPv4 address')
        broker = (str(address), url.port)
    if raw_target:
        target = ipaddress.IPv4Address(raw_target)
        if not target.is_global:
            raise ValueError('target must be a public IPv4 address')
    # No partial grant is used; the service remains available but offline.
    if broker is None or target is None:
        return None
    return (broker, str(target))


def receive_exact(sock, size):
    data = bytearray()
    while len(data) < size:
        part = sock.recv(size - len(data))
        if not part:
            raise OSError('short broker response')
        data.extend(part)
    return bytes(data)


def socks_connect(broker, target):
    sock = socket.create_connection(broker, timeout=TIMEOUT)
    try:
        sock.settimeout(TIMEOUT)
        sock.sendall(b'\x05\x01\x00')
        if receive_exact(sock, 2) != b'\x05\x00':
            raise OSError('broker refused negotiation')
        sock.sendall(b'\x05\x01\x00\x01' + ipaddress.IPv4Address(target).packed + struct.pack('!H', 443))
        head = receive_exact(sock, 4)
        if head[:3] != b'\x05\x00\x00':
            raise OSError('broker refused target')
        if head[3] == 1:
            receive_exact(sock, 4 + 2)
        elif head[3] == 4:
            receive_exact(sock, 16 + 2)
        elif head[3] == 3:
            receive_exact(sock, receive_exact(sock, 1)[0] + 2)
        else:
            raise OSError('invalid broker address type')
        return sock
    except BaseException:
        sock.close()
        raise


def valid_body(body):
    lines = body.splitlines()
    if not lines:
        return False
    seen = set()
    for line in lines:
        if not LINE.fullmatch(line):
            return False
        suffix = line[:35]
        if suffix in seen:
            return False
        seen.add(suffix)
    return True


def fetch_range(config, path, context=None):
    if config is None or not PREFIX.fullmatch(path):
        raise ValueError('unavailable or invalid request')
    broker, target = config
    context = context or ssl.create_default_context(cafile='/etc/ssl/certs/ca-certificates.crt')
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    raw = socks_connect(broker, target)
    try:
        tls = context.wrap_socket(raw, server_hostname=HOST)
    except BaseException:
        raw.close()
        raise
    connection = HTTPConnection(HOST, 443, timeout=TIMEOUT)
    connection.sock = tls
    try:
        connection.request('GET', path, headers={
            'Host': HOST, 'Add-Padding': 'true', 'Connection': 'close',
            'User-Agent': 'ato-fleetbase-breach-check/1',
        })
        response = connection.getresponse()
        if response.status != 200:
            raise OSError('upstream unavailable')
        body = response.read(MAX_BYTES + 1)
        if len(body) > MAX_BYTES or not valid_body(body):
            raise OSError('invalid upstream response')
        return body
    finally:
        connection.close()


class Handler(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, *_args):
        pass

    def send_plain(self, status, data):
        self.send_response(status)
        self.send_header('Content-Type', 'text/plain; charset=utf-8')
        self.send_header('Content-Length', str(len(data)))
        self.send_header('Cache-Control', 'no-store')
        self.send_header('Connection', 'close')
        self.end_headers()
        self.wfile.write(data)
        self.close_connection = True

    def do_GET(self):
        if self.headers.get('Transfer-Encoding') or self.headers.get('Content-Length', '0') != '0':
            self.send_plain(400, b'Invalid request')
            return
        if self.path == '/healthz':
            self.send_plain(200, b'ok')
            return
        if not PREFIX.fullmatch(self.path):
            self.send_plain(404, b'Not found')
            return
        try:
            body = fetch_range(self.server.breach_config, self.path)
        except Exception:
            # Neither a hash prefix nor a raw exception enters logs or output.
            self.send_plain(503, b'Password safety check unavailable')
            return
        self.send_plain(200, body)

    def do_POST(self):
        self.send_plain(405, b'Method not allowed')


class Server(ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 16

    def __init__(self, address, config):
        super().__init__(address, Handler)
        self.breach_config = config
        self.slots = threading.BoundedSemaphore(8)

    def get_request(self):
        sock, address = super().get_request()
        sock.settimeout(TIMEOUT)
        return sock, address

    def process_request(self, request, client_address):
        # Bound connections before creating threads, including slow headers.
        if not self.slots.acquire(blocking=False):
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, client_address)
        except BaseException:
            self.slots.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self.slots.release()

    def handle_error(self, _request, _client_address):
        # BaseServer would log raw exception details from untrusted requests.
        pass


def main():
    config = parse_config(os.environ)
    server = Server(('0.0.0.0', 8081), config)
    def stop(_signum, _frame):
        # shutdown() must run outside the thread executing serve_forever().
        threading.Thread(target=server.shutdown, daemon=True).start()
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    try:
        server.serve_forever(poll_interval=0.5)
    finally:
        server.server_close()


if __name__ == '__main__':
    main()
