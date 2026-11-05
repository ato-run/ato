import http.client
import importlib.util
import ipaddress
from pathlib import Path
import select
import socket
import ssl
import subprocess
import tempfile
import threading
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('breach_proxy', Path(__file__).with_name('breach_proxy.py'))
proxy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(proxy)


class RelayFixture:
    """Local SOCKS and TLS peers; the requested public IP is never contacted."""
    def __init__(self, certificate, key, body=None):
        self.body = body if body is not None else b'A' * 35 + b':5\r\n' + b'B' * 35 + b':0\r\n'
        self.tls = socket.socket()
        self.tls.bind(('127.0.0.1', 0))
        self.tls.listen(1)
        self.broker = socket.socket()
        self.broker.bind(('127.0.0.1', 0))
        self.broker.listen(1)
        self.context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        self.context.load_cert_chain(certificate, key)
        self.request = b''
        self.error = None
        self.threads = [threading.Thread(target=self.serve_tls, daemon=True),
                        threading.Thread(target=self.serve_broker, daemon=True)]
        for thread in self.threads:
            thread.start()

    def serve_tls(self):
        try:
            raw, _ = self.tls.accept()
            with raw, self.context.wrap_socket(raw, server_side=True) as peer:
                while b'\r\n\r\n' not in self.request:
                    chunk = peer.recv(4096)
                    if not chunk:
                        return
                    self.request += chunk
                body = self.body
                peer.sendall(b'HTTP/1.1 200 OK\r\nContent-Length: ' + str(len(body)).encode() +
                             b'\r\nConnection: close\r\n\r\n' + body)
        except (OSError, ssl.SSLError) as exc:
            self.error = exc
        finally:
            self.tls.close()

    def serve_broker(self):
        try:
            client, _ = self.broker.accept()
            with client:
                client.settimeout(5)
                assert proxy.receive_exact(client, 3) == b'\x05\x01\x00'
                client.sendall(b'\x05\x00')
                request = proxy.receive_exact(client, 10)
                assert request == b'\x05\x01\x00\x01' + ipaddress.IPv4Address('1.1.1.1').packed + b'\x01\xbb'
                upstream = socket.create_connection(self.tls.getsockname(), timeout=5)
                with upstream:
                    client.sendall(b'\x05\x00\x00\x01\x00\x00\x00\x00\x00\x00')
                    while True:
                        ready, _, _ = select.select([client, upstream], [], [], 5)
                        if not ready:
                            return
                        for source in ready:
                            data = source.recv(8192)
                            if not data:
                                return
                            (upstream if source is client else client).sendall(data)
        except (OSError, AssertionError) as exc:
            self.error = exc
        finally:
            self.broker.close()

    def close(self):
        for thread in self.threads:
            thread.join(timeout=8)
        if any(thread.is_alive() for thread in self.threads):
            raise AssertionError('fixture did not stop')


class BreachProxyTest(unittest.TestCase):
    def test_binding_requires_exact_grant_and_public_target(self):
        self.assertIsNone(proxy.parse_config({}))
        self.assertIsNone(proxy.parse_config({'ATO_BINDING_BREACH_TARGET': '1.1.1.1'}))
        config = proxy.parse_config({'ATO_BINDING_BREACH_EGRESS': 'socks5://172.18.0.1:1080',
                                     'ATO_BINDING_BREACH_TARGET': '1.1.1.1'})
        self.assertEqual(config, (('172.18.0.1', 1080), '1.1.1.1'))
        for invalid in ('127.0.0.1', '169.254.169.254', '10.1.2.3', 'example.com'):
            with self.subTest(target=invalid), self.assertRaises(ValueError):
                proxy.parse_config({'ATO_BINDING_BREACH_TARGET': invalid})
        for invalid in ('socks5://127.0.0.1:1080', 'socks5://user:pass@172.18.0.1:1080',
                        'socks5://172.18.0.1:1080/path', 'http://172.18.0.1:1080'):
            with self.subTest(broker=invalid), self.assertRaises(ValueError):
                proxy.parse_config({'ATO_BINDING_BREACH_EGRESS': invalid})

    def test_response_validation_fails_closed(self):
        self.assertTrue(proxy.valid_body(b'A' * 35 + b':2\r\n'))
        for value in (b'', b'A' * 34 + b':2', b'A' * 35 + b':bad',
                      (b'A' * 35 + b':2\n') * 2, b'A' * 35 + b':2\nsecret'):
            self.assertFalse(proxy.valid_body(value))

    def test_offline_and_untrusted_requests_never_call_upstream(self):
        server = proxy.Server(('127.0.0.1', 0), None)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with patch.object(proxy, 'fetch_range', side_effect=ValueError('offline')) as fetch:
                for method, path, expected in [('GET', '/healthz', 200), ('GET', '/range/abc12', 404),
                                               ('GET', '/range/ABC12?x=1', 404),
                                               ('POST', '/range/ABC12', 405), ('GET', '/range/ABC12', 503)]:
                    client = http.client.HTTPConnection(*server.server_address, timeout=3)
                    client.request(method, path)
                    response = client.getresponse()
                    self.assertEqual(response.status, expected)
                    response.read()
                    client.close()
                self.assertEqual(fetch.call_count, 1)
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=3)

    def test_verified_tls_over_bounded_socks_path(self):
        root = Path.cwd() / '.tmp'
        root.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='breach-proxy-', dir=root) as temp:
            cert, key = Path(temp) / 'cert.pem', Path(temp) / 'key.pem'
            subprocess.run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
                            '-subj', '/CN=' + proxy.HOST, '-addext', 'subjectAltName=DNS:' + proxy.HOST,
                            '-keyout', str(key), '-out', str(cert)], check=True,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            fixture = RelayFixture(cert, key)
            try:
                context = ssl.create_default_context(cafile=str(cert))
                body = proxy.fetch_range((fixture.broker.getsockname(), '1.1.1.1'), '/range/ABC12', context)
                self.assertTrue(proxy.valid_body(body))
            finally:
                fixture.close()
            self.assertIn(b'GET /range/ABC12 HTTP/1.1', fixture.request)
            self.assertIn(b'Host: api.pwnedpasswords.com', fixture.request)
            self.assertIn(b'Add-Padding: true', fixture.request)

            fixture = RelayFixture(cert, key, b'A' * (proxy.MAX_BYTES + 1))
            try:
                with self.assertRaises(OSError):
                    proxy.fetch_range((fixture.broker.getsockname(), '1.1.1.1'), '/range/ABC12',
                                      ssl.create_default_context(cafile=str(cert)))
            finally:
                fixture.close()

            fixture = RelayFixture(cert, key)
            try:
                with self.assertRaises(ssl.SSLError):
                    proxy.fetch_range((fixture.broker.getsockname(), '1.1.1.1'), '/range/ABC12', ssl.create_default_context())
            finally:
                fixture.close()

            wrong_cert, wrong_key = Path(temp) / 'wrong.pem', Path(temp) / 'wrong-key.pem'
            subprocess.run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
                            '-subj', '/CN=wrong.invalid', '-addext', 'subjectAltName=DNS:wrong.invalid',
                            '-keyout', str(wrong_key), '-out', str(wrong_cert)], check=True,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            fixture = RelayFixture(wrong_cert, wrong_key)
            try:
                with self.assertRaises(ssl.SSLCertVerificationError):
                    proxy.fetch_range((fixture.broker.getsockname(), '1.1.1.1'), '/range/ABC12',
                                      ssl.create_default_context(cafile=str(wrong_cert)))
            finally:
                fixture.close()


if __name__ == '__main__':
    unittest.main()
