from __future__ import annotations

import selectors
import socket
import socketserver
import threading
from dataclasses import dataclass
from urllib.parse import urlsplit

import socks

from .security import validate_connect_target


GOOGLE_API_HOSTS = frozenset({"oauth2.googleapis.com", "gmail.googleapis.com"})


@dataclass(frozen=True)
class ProxyRoute:
    socks_host: str
    socks_port: int
    upstream_host: str
    upstream_port: int
    upstream_authorization: str | None


class _ConnectHandler(socketserver.StreamRequestHandler):
    server: "RestrictedProxyServer"

    def handle(self) -> None:
        request_line = self.rfile.readline(8192).decode("ascii", errors="strict").strip()
        fields = request_line.split()
        if len(fields) != 3 or fields[0] != "CONNECT" or fields[2] != "HTTP/1.1":
            self.wfile.write(b"HTTP/1.1 405 Method Not Allowed\r\nConnection: close\r\n\r\n")
            return
        try:
            target = validate_connect_target(fields[1], self.server.allowed_hosts)
        except ValueError:
            self.wfile.write(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n")
            return
        while True:
            line = self.rfile.readline(8192)
            if line in {b"\r\n", b"\n", b""}:
                break
        upstream = socks.socksocket()
        upstream.set_proxy(
            proxy_type=socks.SOCKS5,
            addr=self.server.route.socks_host,
            port=self.server.route.socks_port,
            rdns=False,
        )
        upstream.settimeout(15)
        try:
            upstream.connect((self.server.route.upstream_host, self.server.route.upstream_port))
            headers = [
                f"CONNECT {target.host}:{target.port} HTTP/1.1",
                f"Host: {target.host}:{target.port}",
                "Proxy-Connection: keep-alive",
            ]
            if self.server.route.upstream_authorization:
                headers.append(
                    "Proxy-Authorization: " + self.server.route.upstream_authorization
                )
            upstream.sendall(("\r\n".join(headers) + "\r\n\r\n").encode("ascii"))
            response = _read_headers(upstream)
            status_line = response.split(b"\r\n", 1)[0]
            if not status_line.startswith(b"HTTP/1.1 200") and not status_line.startswith(
                b"HTTP/1.0 200"
            ):
                self.wfile.write(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n")
                return
            self.wfile.write(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            self.wfile.flush()
            _tunnel(self.connection, upstream)
        finally:
            upstream.close()


def _read_headers(connection: socket.socket) -> bytes:
    value = bytearray()
    while b"\r\n\r\n" not in value:
        chunk = connection.recv(4096)
        if not chunk:
            break
        value.extend(chunk)
        if len(value) > 64 * 1024:
            raise ValueError("upstream proxy response headers are too large")
    return bytes(value)


def _tunnel(left: socket.socket, right: socket.socket) -> None:
    selector = selectors.DefaultSelector()
    selector.register(left, selectors.EVENT_READ, right)
    selector.register(right, selectors.EVENT_READ, left)
    try:
        while True:
            events = selector.select(timeout=60)
            if not events:
                return
            for key, _ in events:
                destination = key.data
                data = key.fileobj.recv(64 * 1024)
                if not data:
                    return
                destination.sendall(data)
    finally:
        selector.close()


class RestrictedProxyServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

    def __init__(
        self, address: tuple[str, int], route: ProxyRoute, allowed_hosts: frozenset[str]
    ) -> None:
        self.route = route
        self.allowed_hosts = allowed_hosts
        super().__init__(address, _ConnectHandler)


def start_restricted_proxy(
    *,
    socks_url: str,
    upstream_authority: str,
    upstream_authorization: str | None,
    additional_allowed_hosts: frozenset[str] = frozenset(),
    listen_port: int = 18080,
) -> RestrictedProxyServer:
    socks_value = urlsplit(socks_url)
    if socks_value.scheme != "socks5" or not socks_value.hostname or not socks_value.port:
        raise ValueError("Ato egress grant must be a socks5 URL")
    if upstream_authority.count(":") != 1:
        raise ValueError("restricted HTTPS proxy must be a numeric host:port")
    upstream_host, port_value = upstream_authority.rsplit(":", 1)
    socket.inet_aton(upstream_host)
    upstream_port = int(port_value)
    if not 1 <= upstream_port <= 65535:
        raise ValueError("restricted HTTPS proxy port is invalid")
    if upstream_authorization and any(
        character in upstream_authorization for character in "\r\n"
    ):
        raise ValueError("restricted HTTPS proxy authorization is invalid")
    route = ProxyRoute(
        socks_host=socks_value.hostname,
        socks_port=socks_value.port,
        upstream_host=upstream_host,
        upstream_port=upstream_port,
        upstream_authorization=upstream_authorization,
    )
    server = RestrictedProxyServer(
        ("127.0.0.1", listen_port), route, GOOGLE_API_HOSTS | additional_allowed_hosts
    )
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server
