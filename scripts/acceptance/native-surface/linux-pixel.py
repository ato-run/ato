#!/usr/bin/env python3
"""Real TigerVNC GUI producer acceptance, intended INSIDE a private Linux sandbox.

Requires Xtigervnc, xauth, xdpyinfo and xterm. No Contract is issued by this test.
The caller supplies a dedicated .tmp root; no host display or TCP X11 is used.
"""
import argparse
import base64
import hmac
import hashlib
import json
import http.client
import os
from pathlib import Path
import signal
import select
import socket
import struct
import subprocess
import time


def exact(stream, count):
    result = bytearray()
    while len(result) < count:
        chunk = stream.recv(count - len(result))
        if not chunk:
            raise RuntimeError("RFB closed before complete frame")
        result.extend(chunk)
    return bytes(result)


class UpgradeRejected(Exception):
    pass


class WebSocketSocket:
    def __init__(self, port, assertion, subprotocol="binary"):
        self.socket = socket.create_connection(("127.0.0.1", port), timeout=4)
        self.socket.settimeout(4)
        key = base64.b64encode(os.urandom(16)).decode()
        request = (f"GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\n"
            f"Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\n"
            f"Sec-WebSocket-Protocol: {subprotocol}\r\nOrigin: http://localhost:5173\r\n"
            f"x-ato-surface-assertion: {assertion}\r\n\r\n")
        self.socket.sendall(request.encode())
        header = bytearray()
        while not header.endswith(b"\r\n\r\n"):
            header.extend(exact(self.socket, 1))
            assert len(header) <= 8192
        if not header.startswith(b"HTTP/1.1 101"):
            self.socket.close()
            raise UpgradeRejected(header.split(b"\r\n", 1)[0].decode())
        accept = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest())
        assert accept in header
        self.pending = bytearray()

    def settimeout(self, value):
        self.socket.settimeout(value)

    def sendall(self, value, opcode=2):
        mask = os.urandom(4)
        length = len(value)
        if length < 126:
            header = bytes([128 | opcode, 128 | length])
        elif length <= 65535:
            header = bytes([128 | opcode, 128 | 126]) + struct.pack(">H", length)
        else:
            header = bytes([128 | opcode, 128 | 127]) + struct.pack(">Q", length)
        self.socket.sendall(header + mask + bytes(byte ^ mask[index % 4] for index, byte in enumerate(value)))

    def read_message(self):
        while True:
            header = exact(self.socket, 2)
            opcode = header[0] & 15
            assert not header[1] & 128
            length = header[1] & 127
            if length == 126:
                length = struct.unpack(">H", exact(self.socket, 2))[0]
            elif length == 127:
                length = struct.unpack(">Q", exact(self.socket, 8))[0]
            assert length <= 1024 * 1024
            payload = exact(self.socket, length)
            if opcode == 8:
                return opcode, payload
            if opcode == 9:
                self.sendall(payload, 10)
                continue
            if opcode == 10:
                continue
            assert opcode in (1, 2)
            return opcode, payload

    def recv(self, count):
        while not self.pending:
            opcode, payload = self.read_message()
            if opcode == 8:
                return b""
            assert opcode == 2
            self.pending.extend(payload)
        value = bytes(self.pending[:count])
        del self.pending[:count]
        return value

    def close(self):
        try:
            self.sendall(b"", 8)
        except OSError:
            pass
        self.socket.close()


def assertion(key, session="native-acceptance", expired=False, execution=None,
              surface="native-pixel", kid="acceptance"):
    claims = {"aud": "ato.runner.surface-gateway", "session_id": session,
        "surface_id": surface, "principal": {"kind": "user", "id": "acceptance"},
        "exp": int(time.time()) + (-1 if expired else 60), "jti": os.urandom(16).hex(), "kid": kid}
    if execution is not None:
        claims["execution"] = execution
    encoded = base64.urlsafe_b64encode(json.dumps(claims, separators=(",", ":")).encode()).decode().rstrip("=")
    return encoded + "." + hmac.new(key.encode(), encoded.encode(), hashlib.sha256).hexdigest()


class Client:
    def __init__(self, port, key=None, execution=None):
        signed = assertion(key, execution=execution,
            kid="ato.runner-surface@1" if execution else "acceptance") if key else None
        self.stream = WebSocketSocket(port, signed) if key else socket.create_connection(("127.0.0.1", port), timeout=4)
        self.stream.settimeout(4)
        assert exact(self.stream, 12) == b"RFB 003.008\n"
        self.stream.sendall(b"RFB 003.008\n")
        options = exact(self.stream, exact(self.stream, 1)[0])
        assert 1 in options
        self.stream.sendall(b"\x01")
        assert exact(self.stream, 4) == b"\0" * 4
        self.stream.sendall(b"\x01")
        header = exact(self.stream, 24)
        self.width, self.height = struct.unpack(">HH", header[:4])
        exact(self.stream, struct.unpack(">I", header[20:24])[0])
        self.stream.sendall(bytes([0, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]))
        self.stream.sendall(struct.pack(">BBHiii", 2, 0, 3, 0, -223, -308))
        self.clipboard_messages = 0

    def request(self):
        self.stream.sendall(struct.pack(">BBHHHH", 3, 1, 0, 0, self.width, self.height))

    def update(self):
        while True:
            kind = exact(self.stream, 1)[0]
            if kind == 2:
                continue
            if kind == 3:
                length = struct.unpack(">I", exact(self.stream, 7)[3:])[0]
                assert length <= 1024 * 1024
                exact(self.stream, length)
                self.clipboard_messages += 1
                continue
            assert kind == 0, kind
            count = struct.unpack(">H", exact(self.stream, 3)[1:])[0]
            pixels = bytearray()
            for _ in range(count):
                x, y, width, height, encoding = struct.unpack(">HHHHi", exact(self.stream, 12))
                if encoding == 0:
                    assert width * height <= 4096 * 2160
                    pixels.extend(exact(self.stream, width * height * 4))
                elif encoding == -223:
                    self.width, self.height = width, height
                elif encoding == -308:
                    screens = exact(self.stream, 4)[0]
                    exact(self.stream, screens * 16)
                    assert y == 0, ("resize refused", y)
                    self.width, self.height = width, height
                else:
                    raise AssertionError(("unsupported test encoding", encoding))
            return bytes(pixels)

    def frame(self):
        for _ in range(10):
            self.request()
            pixels = self.update()
            if pixels:
                return hashlib.sha256(pixels).hexdigest(), len(pixels)
        raise RuntimeError("no pixels")

    def text(self, text):
        for character in text:
            key = 0xFF0D if character == "\n" else ord(character)
            for down in (1, 0):
                self.stream.sendall(struct.pack(">BBHI", 4, down, 0, key))

    def resize(self, width, height):
        self.stream.sendall(struct.pack(">BBHHBBIHHHHI", 251, 0, width, height, 1, 0, 0, 0, 0, width, height, 0))
        self.request()
        for _ in range(10):
            self.update()
            if (self.width, self.height) == (width, height):
                return
            self.request()
        raise AssertionError("resize was not acknowledged")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--producer", type=Path, required=True)
    parser.add_argument("--work-root", type=Path, required=True)
    parser.add_argument("--gateway", type=Path)
    parser.add_argument("--execution-scope", action="store_true")
    parser.add_argument("--application-probe", action="store_true")
    args = parser.parse_args()
    assert not args.execution_scope or args.gateway, "execution scope requires the gateway"
    assert not args.application_probe or (args.gateway and args.execution_scope), "application probe requires scoped gateway"
    execution = {"run_id": "run-native-acceptance", "lease_id": "lease-native-acceptance",
                 "runner_id": "runner-native-acceptance"} if args.execution_scope else None
    kid = "ato.runner-surface@1" if execution else "acceptance"
    root = args.work_root.resolve()
    root.mkdir(parents=True, exist_ok=False)
    environment = dict(os.environ, TMPDIR=str(root), LC_ALL="C", LANG="C")
    port = 15999
    log = root / "producer.log"
    started = time.monotonic()
    with log.open("wb") as output:
        process = subprocess.Popen(["bash", str(args.producer.resolve()), "800", "600", str(port), "199",
            "xterm", "-e", "/bin/bash", "-c",
            'stty -echo; printf "\\033[41mGUI ready\\n"; while IFS= read -r value; do printf "\\033[42mINPUT:%s\\n" "$value"; done'],
            env=environment, stdout=output, stderr=subprocess.STDOUT, start_new_session=True)
        client = None
        gateway = None
        key = None
        application_result = None
        try:
            if args.gateway:
                if args.application_probe:
                    # Wait for the private producer before giving its owned
                    # endpoint to the one-shot operation Adapter.
                    deadline = time.monotonic() + 8
                    while True:
                        try:
                            private = Client(port)
                            private.frame()
                            private.stream.close()
                            break
                        except (OSError, RuntimeError):
                            assert time.monotonic() < deadline, "private GUI did not start"
                            time.sleep(0.05)
                    time.sleep(0.15)
                key = os.urandom(32).hex()
                gateway = subprocess.Popen([str(args.gateway)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=output, text=True, env=environment)
                configuration = {"session_id": "native-acceptance", "surface_id": "native-pixel",
                    "listen_addr": "127.0.0.1:16000", "connect_url": "http://127.0.0.1:16000/",
                    "origin": "http://localhost:5173", "assertion_keys": {kid: key},
                    "idle_timeout_secs": 30, "readiness_timeout_secs": 8,
                    "target": {"kind": "pixel", "private_rfb_addr": f"127.0.0.1:{port}"}}
                if execution:
                    configuration["execution"] = execution
                if args.application_probe:
                    configuration["application_probe"] = {"listen_addr": "127.0.0.1:16001", "adapter": {
                        "surface": {"kind": "pixel_stream", "port": "native", "width": 800, "height": 600},
                        "probe_port": "native-results", "operations": [{"kind": "pixel_key_frame",
                            "path": "/result", "keysyms": [ord(c) for c in "probe"] + [0xFF0D],
                            "max_frame_bytes": 800 * 600 * 4, "timeout_ms": 5000}]}}
                gateway.stdin.write(json.dumps(configuration) + "\n")
                gateway.stdin.flush()
                assert select.select([gateway.stdout], [], [], 10)[0], "gateway readiness timed out"
                ready = json.loads(gateway.stdout.readline())
                assert ready["surface_ready"] and not ready["contract_verified"]
                if args.application_probe:
                    for _ in range(2):
                        observed = http.client.HTTPConnection("127.0.0.1", 16001, timeout=3)
                        observed.request("GET", "/result")
                        response = observed.getresponse()
                        assert response.status == 200
                        body = response.read(1024)
                        assert len(body) == 72 and body.startswith(b"sha256:") and body.endswith(b"\n")
                        assert len(bytes.fromhex(body[7:-1].decode())) == 32
                        if application_result is not None:
                            assert application_result == body.decode()
                        application_result = body.decode()
                        observed.close()
                port = 16000
                invalid = [assertion(key, expired=True, execution=execution, kid=kid),
                           assertion(key, session="foreign-session", execution=execution, kid=kid)]
                if execution:
                    invalid.extend(assertion(key, execution=dict(execution, **{field: "foreign"}), kid=kid)
                                   for field in execution)
                    invalid.append(assertion(key, kid=kid))
                for rejected in invalid:
                    try:
                        WebSocketSocket(port, rejected)
                        raise AssertionError("invalid assertion admitted")
                    except UpgradeRejected as error:
                        assert "401" in str(error)

            while time.monotonic() - started < 8:
                if process.poll() is not None:
                    raise AssertionError("GUI producer exited: " + log.read_text())
                try:
                    client = Client(port, key, execution)
                    break
                except (OSError, RuntimeError):
                    time.sleep(0.1)
            assert client is not None
            time.sleep(0.5)
            first, frame_bytes = client.frame()
            ready_ms = round((time.monotonic() - started) * 1000)
            client.text("hello\n")
            time.sleep(0.5)
            changed, _ = client.frame()
            assert first != changed, "keyboard input did not change GUI pixels"
            client.resize(1024, 768)
            resized, resized_bytes = client.frame()
            client.stream.close()
            client = Client(port, key, execution)
            reconnect, _ = client.frame()
            assert (client.width, client.height) == (1024, 768)
            assert client.clipboard_messages == 0
            report = {"schema": "ato.native-pixel-producer-acceptance/1", "real_tigervnc": True, "transport": "authenticated_gateway" if key else "private_rfb",
                "expired_assertion_rejected": bool(key), "wrong_session_rejected": bool(key),
                "execution_scope_verified": bool(execution),
                "application_probe_result": application_result,
                "initial_frame_bytes": frame_bytes, "ready_ms": ready_ms, "keyboard_changed_pixels": True,
                "resized_viewport": [client.width, client.height], "resized_frame_bytes": resized_bytes,
                "reconnect": True, "clipboard_messages": client.clipboard_messages,
                "contract_verified": False, "frame_sha256": [first, changed, resized, reconnect]}
        finally:
            if client is not None:
                client.stream.close()
            if gateway is not None:
                gateway.stdin.close()
                gateway.wait(timeout=5)
                assert gateway.returncode == 0
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
            process.wait(timeout=5)
    with socket.socket() as stopped:
        assert stopped.connect_ex(("127.0.0.1", port)) != 0, "RFB survived producer shutdown"
    with socket.socket() as stopped:
        assert stopped.connect_ex(("127.0.0.1", 15999)) != 0, "producer survived shutdown"
    report["shutdown_listener_closed"] = True
    (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report))


if __name__ == "__main__":
    main()
