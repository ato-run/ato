#!/usr/bin/env python3
"""Run Browser Adapter acceptance with a real Chrome process over raw CDP.

The default mode is the original operator harness.  ``--browser-host`` instead
starts Ato's host-private Browser Host, which reads runtime discovery and
performs the isolated-world injection itself.  Both modes drive trusted input
through Chrome's CDP Input domain; neither uses Playwright.
"""

from __future__ import annotations

import argparse
import atexit
import base64
import hashlib
import http.client
import json
import os
import re
import secrets
import shutil
import socket
import struct
import subprocess
import sys
import time
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any


PROTOCOL = "ato.browser@1"
TIMEOUT_SECONDS = 30
FAILURE_TIMEOUT_SECONDS = 45


class AcceptanceError(RuntimeError):
    pass


class WebSocket:
    def __init__(self, url: str):
        parsed = urllib.parse.urlparse(url)
        if parsed.scheme != "ws" or parsed.hostname not in {"127.0.0.1", "localhost"}:
            raise AcceptanceError("CDP endpoint must be a loopback ws:// URL")
        self.socket = socket.create_connection(
            (parsed.hostname, parsed.port or 80), timeout=TIMEOUT_SECONDS
        )
        self.socket.settimeout(TIMEOUT_SECONDS)
        key = base64.b64encode(secrets.token_bytes(16)).decode("ascii")
        target = parsed.path or "/"
        if parsed.query:
            target += f"?{parsed.query}"
        request = (
            f"GET {target} HTTP/1.1\r\n"
            f"Host: {parsed.hostname}:{parsed.port or 80}\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n\r\n"
        )
        self.socket.sendall(request.encode("ascii"))
        response = self._read_until(b"\r\n\r\n")
        status = response.split(b"\r\n", 1)[0]
        if b" 101 " not in status:
            raise AcceptanceError(f"CDP WebSocket handshake failed: {status!r}")
        expected = base64.b64encode(
            hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()
        )
        headers = {
            line.split(b":", 1)[0].strip().lower(): line.split(b":", 1)[1].strip()
            for line in response.split(b"\r\n")[1:]
            if b":" in line
        }
        if headers.get(b"sec-websocket-accept") != expected:
            raise AcceptanceError("CDP WebSocket accept hash mismatch")

    def _read_until(self, delimiter: bytes) -> bytes:
        value = bytearray()
        while delimiter not in value:
            chunk = self.socket.recv(4096)
            if not chunk:
                raise AcceptanceError("CDP WebSocket closed during handshake")
            value.extend(chunk)
        return bytes(value)

    def _read_exact(self, length: int) -> bytes:
        value = bytearray()
        while len(value) < length:
            chunk = self.socket.recv(length - len(value))
            if not chunk:
                raise AcceptanceError("CDP WebSocket disconnected")
            value.extend(chunk)
        return bytes(value)

    def send_json(self, value: dict[str, Any]) -> None:
        payload = json.dumps(value, separators=(",", ":")).encode()
        mask = secrets.token_bytes(4)
        length = len(payload)
        if length < 126:
            header = bytes((0x81, 0x80 | length))
        elif length <= 0xFFFF:
            header = bytes((0x81, 0x80 | 126)) + struct.pack("!H", length)
        else:
            header = bytes((0x81, 0x80 | 127)) + struct.pack("!Q", length)
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        self.socket.sendall(header + mask + masked)

    def receive_json(self) -> dict[str, Any]:
        while True:
            first, second = self._read_exact(2)
            opcode = first & 0x0F
            length = second & 0x7F
            if length == 126:
                length = struct.unpack("!H", self._read_exact(2))[0]
            elif length == 127:
                length = struct.unpack("!Q", self._read_exact(8))[0]
            mask = self._read_exact(4) if second & 0x80 else None
            payload = self._read_exact(length)
            if mask is not None:
                payload = bytes(
                    byte ^ mask[index % 4] for index, byte in enumerate(payload)
                )
            if opcode == 0x8:
                raise AcceptanceError("CDP WebSocket closed")
            if opcode == 0x9:
                self._send_control(0xA, payload)
                continue
            if opcode != 0x1:
                continue
            return json.loads(payload)

    def _send_control(self, opcode: int, payload: bytes) -> None:
        mask = secrets.token_bytes(4)
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        self.socket.sendall(bytes((0x80 | opcode, 0x80 | len(payload))) + mask + masked)

    def close(self) -> None:
        try:
            self._send_control(0x8, b"")
        except OSError:
            pass
        self.socket.close()


class Cdp:
    def __init__(self, url: str):
        self.websocket = WebSocket(url)
        self.next_id = 1
        self.events: list[dict[str, Any]] = []

    def call(
        self, method: str, params: dict[str, Any] | None = None, session_id: str | None = None
    ) -> dict[str, Any]:
        request_id = self.next_id
        self.next_id += 1
        message: dict[str, Any] = {"id": request_id, "method": method}
        if params is not None:
            message["params"] = params
        if session_id is not None:
            message["sessionId"] = session_id
        self.websocket.send_json(message)
        while True:
            response = self.websocket.receive_json()
            if response.get("id") == request_id:
                if "error" in response:
                    raise AcceptanceError(
                        f"CDP {method} failed: {response['error'].get('message', response['error'])}"
                    )
                return response.get("result", {})
            self.events.append(response)

    def close(self) -> None:
        self.websocket.close()


class ChromeSession:
    def __init__(
        self,
        chrome_binary: Path,
        work_dir: Path,
        bridge_source: str,
        bootstrap: dict[str, Any],
        origin: str,
        delay_ack: bool,
        disconnect_on_apply: bool = False,
        browser_host: tuple[Path, Path] | None = None,
    ):
        self.closed = False
        self.cdp: Cdp | None = None
        self.browser_context_id: str | None = None
        self.browser_host = browser_host is not None
        self.work_dir = work_dir
        self.log_file = (work_dir / "chrome.log").open("wb")
        if browser_host is not None:
            ato, runtime_dir = browser_host
            self.profile = runtime_dir / "browser-host-profile"
            self.process = subprocess.Popen(
                [
                    str(ato),
                    "__browser-host",
                    "--runtime-dir",
                    str(runtime_dir),
                    "--target-url",
                    origin,
                    "--chrome",
                    str(chrome_binary),
                    "--headless",
                ],
                stdout=self.log_file,
                stderr=subprocess.STDOUT,
            )
            atexit.register(self.close)
            self.debug_port = wait_for_browser_host_debug_port(self.profile)
            version = wait_for_json(f"http://127.0.0.1:{self.debug_port}/json/version")
            self.version = version["Browser"]
            self.cdp = Cdp(version["webSocketDebuggerUrl"])
            target = wait_until(
                lambda: next(
                    (
                        value
                        for value in self.cdp.call("Target.getTargets")["targetInfos"]
                        if value.get("type") == "page"
                        and value.get("url", "").startswith(origin)
                    ),
                    None,
                )
            )
            attached = self.cdp.call(
                "Target.attachToTarget", {"targetId": target["targetId"], "flatten": True}
            )
            self.session_id = attached["sessionId"]
            self.cdp.call("Runtime.enable", session_id=self.session_id)
            self.cdp.call("Page.enable", session_id=self.session_id)
            self.wait_document_ready()
            return

        self.profile = work_dir / "profile"
        self.profile.mkdir(parents=True)
        self.debug_port = unused_port()
        self.process = subprocess.Popen(
            [
                str(chrome_binary),
                "--headless=new",
                "--disable-gpu",
                "--disable-dev-shm-usage",
                "--no-first-run",
                "--no-default-browser-check",
                "--remote-debugging-address=127.0.0.1",
                f"--remote-debugging-port={self.debug_port}",
                "--remote-allow-origins=*",
                "--window-size=800,600",
                f"--user-data-dir={self.profile}",
                "about:blank",
            ],
            stdout=self.log_file,
            stderr=subprocess.STDOUT,
        )
        atexit.register(self.close)
        version = wait_for_json(f"http://127.0.0.1:{self.debug_port}/json/version")
        self.version = version["Browser"]
        self.cdp = Cdp(version["webSocketDebuggerUrl"])
        context = self.cdp.call("Target.createBrowserContext")
        self.browser_context_id = context["browserContextId"]
        target = self.cdp.call(
            "Target.createTarget",
            {"url": "about:blank", "browserContextId": self.browser_context_id},
        )
        attached = self.cdp.call(
            "Target.attachToTarget", {"targetId": target["targetId"], "flatten": True}
        )
        self.session_id = attached["sessionId"]
        self.cdp.call("Runtime.enable", session_id=self.session_id)
        self.cdp.call("Page.enable", session_id=self.session_id)
        if delay_ack:
            original = 'send({ type: "ack", request_id: value.request_id });'
            replacement = (
                'setTimeout(() => send({ type: "ack", request_id: value.request_id }), 200);'
            )
            if original not in bridge_source:
                raise AcceptanceError("Bridge ACK hook not found")
            bridge_source = bridge_source.replace(original, replacement)
        if disconnect_on_apply:
            original = "dispatch(value.event);"
            replacement = (
                'socket.close(4000, "injected staging replay disconnect"); return;'
            )
            if original not in bridge_source:
                raise AcceptanceError("Bridge dispatch hook not found")
            bridge_source = bridge_source.replace(original, replacement)
        world_name = f"ato.browser.bridge.{bootstrap['browser_session']}"
        source = (
            f"globalThis.__ATO_BROWSER_BOOTSTRAP__ = {json.dumps(bootstrap)};\n"
            f"{bridge_source}"
        )
        self.cdp.call(
            "Page.addScriptToEvaluateOnNewDocument",
            {"source": source, "worldName": world_name},
            self.session_id,
        )
        self.cdp.call("Page.navigate", {"url": origin}, self.session_id)
        self.wait_document_ready()

    def evaluate(self, expression: str, context_id: int | None = None) -> Any:
        params: dict[str, Any] = {
            "expression": expression,
            "returnByValue": True,
            "awaitPromise": True,
        }
        if context_id is not None:
            params["contextId"] = context_id
        result = self.cdp.call("Runtime.evaluate", params, self.session_id)
        if "exceptionDetails" in result:
            raise AcceptanceError(
                f"Chrome evaluation failed: {result['exceptionDetails'].get('text', 'exception')}"
            )
        return result.get("result", {}).get("value")

    def wait_document_ready(self) -> None:
        wait_until(lambda: self.evaluate("document.readyState") in {"interactive", "complete"})

    def isolated_context(self) -> int:
        expected = f"ato.browser.bridge."

        def find() -> int | None:
            for index, event in enumerate(self.cdp.events):
                if event.get("method") == "Runtime.executionContextCreated":
                    context = event["params"]["context"]
                    if context.get("name", "").startswith(expected):
                        self.cdp.events.pop(index)
                        return context["id"]
            self.cdp.call("Runtime.evaluate", {"expression": "0"}, self.session_id)
            return None

        return wait_until(find)

    def click(self, selector: str) -> None:
        rect = self.evaluate(
            """(() => {
              const value = document.querySelector(%s);
              if (!value) return null;
              const rect = value.getBoundingClientRect();
              return {x: rect.x + rect.width / 2, y: rect.y + rect.height / 2};
            })()"""
            % json.dumps(selector)
        )
        if not rect:
            raise AcceptanceError(f"Chrome target not found: {selector}")
        common = {
            "x": rect["x"],
            "y": rect["y"],
            "button": "left",
            "clickCount": 1,
            "pointerType": "mouse",
        }
        self.cdp.call(
            "Input.dispatchMouseEvent", {"type": "mousePressed", **common, "buttons": 1}, self.session_id
        )
        self.cdp.call(
            "Input.dispatchMouseEvent", {"type": "mouseReleased", **common, "buttons": 0}, self.session_id
        )

    def close(self) -> None:
        if self.closed:
            return
        self.closed = True
        atexit.unregister(self.close)
        if self.cdp is not None:
            try:
                if self.browser_context_id is not None and not self.browser_host:
                    self.cdp.call(
                        "Target.disposeBrowserContext",
                        {"browserContextId": self.browser_context_id},
                    )
            except (AcceptanceError, OSError):
                pass
            try:
                self.cdp.close()
            except OSError:
                pass
        if self.process.poll() is None:
            self.process.terminate()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
        self.log_file.close()


def unused_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def wait_for_browser_host_debug_port(profile: Path) -> int:
    def read() -> int | None:
        try:
            value = (profile / "DevToolsActivePort").read_text().splitlines()[0]
            return int(value)
        except (FileNotFoundError, IndexError, ValueError):
            return None

    return wait_until(read)


def wait_until(predicate, timeout: float = TIMEOUT_SECONDS):
    deadline = time.monotonic() + timeout
    last_error: Exception | None = None
    while time.monotonic() < deadline:
        try:
            result = predicate()
            if result:
                return result
        except Exception as error:  # readiness polling records the final cause
            last_error = error
        time.sleep(0.05)
    suffix = f": {last_error}" if last_error else ""
    raise AcceptanceError(f"condition did not become true within {timeout}s{suffix}")


def wait_for_json(url: str) -> dict[str, Any]:
    def read() -> dict[str, Any] | None:
        with urllib.request.urlopen(url, timeout=1) as response:
            return json.load(response)

    return wait_until(read)


def wait_for_bootstrap(runtime_dir: Path) -> tuple[Path, dict[str, Any]]:
    def read() -> tuple[Path, dict[str, Any]] | None:
        matches = list(runtime_dir.glob("browser-*.json"))
        if len(matches) != 1:
            return None
        return matches[0], json.loads(matches[0].read_text())

    return wait_until(read)


def wait_for_portable_project(home: Path) -> Path:
    def find() -> Path | None:
        cache = home / "cache"
        if not cache.is_dir():
            return None
        for candidate in sorted(cache.glob("portable-run-*/workspace")):
            if (candidate / ".capsule").is_dir():
                return candidate
        return None

    return wait_until(find)


def run_ato(ato: Path, arguments: list[str], home: Path, runtime_dir: Path) -> subprocess.CompletedProcess:
    return subprocess.run(
        [str(ato), *arguments],
        check=True,
        text=True,
        capture_output=True,
        env={**os.environ, "ATO_HOME": str(home), "ATO_BROWSER_RUNTIME_DIR": str(runtime_dir)},
        timeout=TIMEOUT_SECONDS,
    )


def stop_child(process: subprocess.Popen, port: int) -> None:
    if process.poll() is not None:
        return
    try:
        connection = http.client.HTTPConnection("127.0.0.1", port, timeout=1)
        connection.request("GET", "/__shutdown")
        connection.getresponse().read()
        connection.close()
        process.wait(timeout=5)
        return
    except (OSError, subprocess.TimeoutExpired):
        pass
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def server_state(port: int) -> dict[str, Any]:
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
    connection.request("GET", "/__ato_test/state")
    response = connection.getresponse()
    value = json.loads(response.read())
    connection.close()
    return value


def server_is_closed(port: int) -> bool:
    with socket.socket() as connection:
        connection.settimeout(0.2)
        return connection.connect_ex(("127.0.0.1", port)) != 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--ato", type=Path, required=True)
    parser.add_argument("--chrome", type=Path, required=True)
    parser.add_argument("--repository", type=Path, required=True)
    parser.add_argument("--work-root", type=Path, required=True)
    parser.add_argument("--receipt", type=Path, required=True)
    parser.add_argument(
        "--browser-host",
        action="store_true",
        help="exercise the internal Browser Host instead of operator injection",
    )
    args = parser.parse_args()
    work_root = args.work_root.resolve()
    if work_root.exists():
        raise AcceptanceError(f"work root already exists: {work_root}")
    work_root.mkdir(parents=True, mode=0o700)
    project = work_root / "project"
    project.mkdir()
    author_home = work_root / "author-home"
    recipient_home = work_root / "recipient-home"
    author_runtime = work_root / "author-private-runtime"
    recipient_runtime = work_root / "recipient-private-runtime"
    for directory in [author_home, recipient_home, author_runtime, recipient_runtime]:
        directory.mkdir(mode=0o700)
    fixture_root = args.repository / "tests/browser/fixtures"
    shutil.copy(
        fixture_root / "http-stateful-counter/index.html", project / "index.html"
    )
    (project / "server.py").write_text(
        '''from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from threading import Thread
import json
import sys

count = 0
requests = 0

class Handler(BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        pass

    def send_value(self, status, content_type, body):
        self.send_response(status)
        self.send_header("content-type", content_type)
        self.send_header("content-length", str(len(body)))
        self.send_header("connection", "close")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/__ato_test/state":
            body = json.dumps({"count": count, "requests": requests}).encode()
            self.send_value(200, "application/json", body)
            return
        if self.path == "/__shutdown":
            self.send_value(204, "text/plain", b"")
            Thread(target=self.server.shutdown, daemon=True).start()
            return
        body = Path("index.html").read_bytes()
        self.send_value(200, "text/html; charset=utf-8", body)

    def do_POST(self):
        global count, requests
        if self.path != "/increment":
            self.send_value(404, "text/plain", b"")
            return
        count += 1
        requests += 1
        body = json.dumps({"count": count, "requests": requests}).encode()
        self.send_value(200, "application/json", body)

server = ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler)
server.serve_forever()
'''
    )
    bridge_source = (
        args.repository / "extensions/adapters/browser/bridge/browser-bridge.js"
    ).read_text()
    port = unused_port()
    origin = f"http://127.0.0.1:{port}"
    (project / "capsule.toml").write_text(
        f'''schema = 1

[[process]]
id = "app"
command = ["{sys.executable}", "server.py", "{port}"]
cwd = "."

[[adapter]]
target = "app"
use = "ato.process@1"

[[adapter]]
port = "app.browser"
use = "ato.browser@1"

[[port]]
id = "app.browser"
node = "app"
protocol = "ato.browser@1"
role = "server"

[adapter.config]
expected_origin = "{origin}"

[encap]
materializers = ["ato.replay@1"]
'''
    )
    run_ato(args.ato, ["init", str(project)], author_home, author_runtime)
    stop_author = lambda: subprocess.run(
        [str(args.ato), "stop", str(project)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        env={
            **os.environ,
            "ATO_HOME": str(author_home),
            "ATO_BROWSER_RUNTIME_DIR": str(author_runtime),
        },
        timeout=10,
    )
    atexit.register(stop_author)
    initial_head = (project / ".capsule/refs/heads/main").read_text().strip()
    _, author_bootstrap = wait_for_bootstrap(author_runtime)
    wait_until(lambda: server_state(port))
    author_chrome = ChromeSession(
        args.chrome,
        work_root / "author-chrome",
        bridge_source,
        author_bootstrap,
        origin,
        False,
        browser_host=(args.ato, author_runtime) if args.browser_host else None,
    )
    author_context = author_chrome.isolated_context()
    wait_until(
        lambda: author_chrome.evaluate(
            "globalThis.__ATO_BROWSER_LIFECYCLE__", author_context
        )
        == "active"
    )
    author_chrome.click("#increment")
    wait_until(lambda: server_state(port).get("count") == 1)
    author_chrome.click("#increment")
    wait_until(lambda: server_state(port).get("count") == 2)
    if not args.browser_host:
        author_chrome.close()
    run_ato(args.ato, ["stop", str(project)], author_home, author_runtime)
    if args.browser_host:
        author_chrome.close()
    atexit.unregister(stop_author)
    if list(author_runtime.glob("browser-*.json")):
        raise AcceptanceError("author runtime discovery was not cleaned")
    final_head = (project / ".capsule/refs/heads/main").read_text().strip()
    if final_head == initial_head:
        raise AcceptanceError("Browser input did not advance the ComputationRef")
    records = [
        json.loads(path.read_text())
        for path in sorted((project / ".capsule/records/main").glob("*.json"))
    ]
    browser_records = [record for record in records if record["adapter_id"] == PROTOCOL]
    if not browser_records or any(
        record["protocol_id"] != PROTOCOL or record["head_before"] == record["head_after"]
        for record in browser_records
    ):
        raise AcceptanceError("Browser Record semantic evidence is incomplete")
    bundle = work_root / "counter.capsule"
    run_ato(
        args.ato,
        ["encap", f"{project}@main", "--materialize", "ato.replay@1", "-o", str(bundle)],
        author_home,
        author_runtime,
    )
    portable = json.loads(bundle.read_text())
    portable_text = bundle.read_text()
    for secret_value in [
        author_bootstrap["channel_credential"],
        author_bootstrap["browser_session"],
        author_bootstrap["control_url"],
    ]:
        if secret_value in portable_text:
            raise AcceptanceError("runtime Browser credential leaked into portable Capsule")
    recipient_process = subprocess.Popen(
        [str(args.ato), "run", str(bundle)],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env={
            **os.environ,
            "ATO_HOME": str(recipient_home),
            "ATO_BROWSER_RUNTIME_DIR": str(recipient_runtime),
        },
    )
    atexit.register(stop_child, recipient_process, port)
    _, recipient_bootstrap = wait_for_bootstrap(recipient_runtime)
    recipient_project = wait_for_portable_project(recipient_home)
    wait_until(lambda: server_state(port))
    recipient_chrome = ChromeSession(
        args.chrome,
        work_root / "recipient-chrome",
        bridge_source,
        recipient_bootstrap,
        origin,
        True,
        browser_host=(args.ato, recipient_runtime) if args.browser_host else None,
    )
    recipient_context = recipient_chrome.isolated_context()
    if not args.browser_host:
        wait_until(
            lambda: recipient_chrome.evaluate(
                "globalThis.__ATO_BROWSER_LIFECYCLE__", recipient_context
            )
            == "restoring"
        )
        recipient_chrome.click("#increment")
        time.sleep(0.1)
        if server_state(port).get("count") != 0:
            raise AcceptanceError("real Chrome input reached the app during Replay")
    wait_until(
        lambda: recipient_chrome.evaluate(
            "globalThis.__ATO_BROWSER_LIFECYCLE__", recipient_context
        )
        == "active"
    )
    wait_until(lambda: server_state(port).get("count") == 2)
    wait_until(
        lambda: recipient_chrome.evaluate(
            "Number(document.querySelector('#count').textContent)"
        )
        == 2
    )
    page_security = recipient_chrome.evaluate(
        """({
          globals: Object.keys(globalThis).filter((key) => key.startsWith('__ATO_BROWSER_')),
          local: localStorage.length,
          session: sessionStorage.length,
          count: Number(document.querySelector('#count').textContent)
        })"""
    )
    if page_security != {"globals": [], "local": 0, "session": 0, "count": 2}:
        raise AcceptanceError(f"page-realm security assertion failed: {page_security}")
    recipient_chrome.click("#increment")
    def continued_state() -> dict[str, Any] | None:
        value = server_state(port)
        return value if value.get("count") == 3 else None

    final_state = wait_until(continued_state)
    recipient_chrome.evaluate("fetch('/__shutdown')")
    if not args.browser_host:
        recipient_chrome.close()
    stdout, stderr = recipient_process.communicate(timeout=TIMEOUT_SECONDS)
    atexit.unregister(stop_child)
    if args.browser_host:
        recipient_chrome.close()
    if recipient_process.returncode != 0:
        raise AcceptanceError(f"portable Run failed: {stderr}")
    wait_until(lambda: not list(recipient_runtime.glob("browser-*.json")))
    continued_match = re.search(r"continued computation: (blake3:[0-9a-f]+)", stderr)
    workspace_match = re.search(r"continuation workspace: (.+)", stderr)
    if continued_match is None or workspace_match is None:
        raise AcceptanceError(f"portable continuation receipt is missing: {stderr}")
    continued_head = continued_match.group(1)
    if Path(workspace_match.group(1)) != recipient_project:
        raise AcceptanceError("retained continuation workspace does not match the recipient Run")
    if (recipient_project / ".capsule/refs/heads/continued").read_text().strip() != continued_head:
        raise AcceptanceError("continued branch head does not match the portable Run receipt")
    if continued_head == final_head:
        raise AcceptanceError("post-Replay human input did not create a new ComputationRef")
    continued_records = [
        json.loads(path.read_text())
        for path in sorted(
            (recipient_project / ".capsule/records/continued").glob("*.json")
        )
    ]
    if not continued_records or continued_records[-1]["head_after"] != continued_head:
        raise AcceptanceError("continued Browser Records do not seal the new future")
    continued_bundle = work_root / "continued.capsule"
    run_ato(
        args.ato,
        [
            "encap",
            f"{recipient_project}@continued",
            "--materialize",
            "ato.replay@1",
            "-o",
            str(continued_bundle),
        ],
        recipient_home,
        recipient_runtime,
    )
    continued_portable = json.loads(continued_bundle.read_text())
    if continued_portable["index"]["root"] != continued_head:
        raise AcceptanceError("continued future was not exported at its sealed head")
    continued_text = continued_bundle.read_text()
    for secret_value in [
        recipient_bootstrap["channel_credential"],
        recipient_bootstrap["browser_session"],
        recipient_bootstrap["control_url"],
    ]:
        if secret_value in continued_text:
            raise AcceptanceError("recipient Browser credential leaked into continued Capsule")

    mixed_project = work_root / "browser-http-project"
    mixed_project.mkdir()
    shutil.copy(project / "index.html", mixed_project / "index.html")
    shutil.copy(project / "server.py", mixed_project / "server.py")
    mixed_home = work_root / "browser-http-home"
    mixed_runtime = work_root / "browser-http-private-runtime"
    mixed_home.mkdir(mode=0o700)
    mixed_runtime.mkdir(mode=0o700)
    mixed_upstream_port = unused_port()
    mixed_public_port = unused_port()
    mixed_origin = f"http://127.0.0.1:{mixed_public_port}"
    (mixed_project / "capsule.toml").write_text(
        f'''schema = 1

[[process]]
id = "app"
command = ["{sys.executable}", "server.py", "{mixed_upstream_port}"]
cwd = "."

[[adapter]]
target = "app"
use = "ato.process@1"

[[port]]
id = "app.browser"
node = "app"
protocol = "ato.browser@1"
role = "server"

[[adapter]]
port = "app.browser"
use = "ato.browser@1"

[adapter.config]
expected_origin = "{mixed_origin}"

[[port]]
id = "app.http"
node = "app"
protocol = "ato.http@1"
role = "server"

[[adapter]]
port = "app.http"
use = "ato.http@1"
listen = "127.0.0.1:{mixed_public_port}"
upstream = "127.0.0.1:{mixed_upstream_port}"
ready_path = "/"

[encap]
materializers = ["ato.replay@1"]
'''
    )
    run_ato(args.ato, ["init", str(mixed_project)], mixed_home, mixed_runtime)
    stop_mixed = lambda: subprocess.run(
        [str(args.ato), "stop", str(mixed_project)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        env={
            **os.environ,
            "ATO_HOME": str(mixed_home),
            "ATO_BROWSER_RUNTIME_DIR": str(mixed_runtime),
        },
        timeout=10,
    )
    atexit.register(stop_mixed)
    _, mixed_bootstrap = wait_for_bootstrap(mixed_runtime)
    wait_until(lambda: server_state(mixed_public_port))
    mixed_chrome = ChromeSession(
        args.chrome,
        work_root / "browser-http-chrome",
        bridge_source,
        mixed_bootstrap,
        mixed_origin,
        False,
        browser_host=(args.ato, mixed_runtime) if args.browser_host else None,
    )
    mixed_context = mixed_chrome.isolated_context()
    wait_until(
        lambda: mixed_chrome.evaluate(
            "globalThis.__ATO_BROWSER_LIFECYCLE__", mixed_context
        )
        == "active"
    )
    mixed_chrome.click("#increment")
    mixed_state = wait_until(
        lambda: (
            value
            if (value := server_state(mixed_public_port)).get("count") == 1
            else None
        )
    )
    if not args.browser_host:
        mixed_chrome.close()
    run_ato(args.ato, ["stop", str(mixed_project)], mixed_home, mixed_runtime)
    atexit.unregister(stop_mixed)
    if args.browser_host:
        mixed_chrome.close()
    mixed_records = [
        json.loads(path.read_text())
        for path in sorted((mixed_project / ".capsule/records/main").glob("*.json"))
    ]
    mixed_adapters = {record["adapter_id"] for record in mixed_records}
    if not {PROTOCOL, "ato.http@1"}.issubset(mixed_adapters):
        raise AcceptanceError("mixed Browser + HTTP run did not record both boundaries")
    mixed_bundle = work_root / "browser-http.capsule"
    mixed_encap = subprocess.run(
        [
            str(args.ato),
            "encap",
            f"{mixed_project}@main",
            "--materialize",
            "ato.replay@1",
            "-o",
            str(mixed_bundle),
        ],
        text=True,
        capture_output=True,
        env={
            **os.environ,
            "ATO_HOME": str(mixed_home),
            "ATO_BROWSER_RUNTIME_DIR": str(mixed_runtime),
        },
        timeout=TIMEOUT_SECONDS,
    )
    mixed_diagnostic = (
        "Browser-driven network effects cannot currently be replayed through both "
        "Browser and HTTP adapters"
    )
    if mixed_encap.returncode == 0 or mixed_diagnostic not in mixed_encap.stderr:
        raise AcceptanceError(
            f"mixed Browser + HTTP Replay did not fail closed: {mixed_encap.stderr}"
        )
    if mixed_bundle.exists() or mixed_state != {"count": 1, "requests": 1}:
        raise AcceptanceError("mixed Browser + HTTP path duplicated its server mutation")
    wait_until(lambda: not list(mixed_runtime.glob("browser-*.json")))
    failure_home = work_root / "failure-home"
    failure_runtime = work_root / "failure-private-runtime"
    failure_home.mkdir(mode=0o700)
    failure_runtime.mkdir(mode=0o700)
    failure_stdout_path = work_root / "failure-stdout.log"
    failure_stderr_path = work_root / "failure-stderr.log"
    failure_stdout_handle = failure_stdout_path.open("wb")
    failure_stderr_handle = failure_stderr_path.open("wb")
    failure_process = subprocess.Popen(
        [str(args.ato), "run", str(bundle)],
        stdout=failure_stdout_handle,
        stderr=failure_stderr_handle,
        env={
            **os.environ,
            "ATO_HOME": str(failure_home),
            "ATO_BROWSER_RUNTIME_DIR": str(failure_runtime),
        },
    )
    atexit.register(stop_child, failure_process, port)
    _, failure_bootstrap = wait_for_bootstrap(failure_runtime)
    wait_until(lambda: server_state(port))
    failure_chrome = ChromeSession(
        args.chrome,
        work_root / "failure-chrome",
        bridge_source,
        failure_bootstrap,
        origin,
        False,
        True,
        # The failure injection modifies the test Bridge source. Browser Host
        # deliberately never accepts injected application/Bridge source, so
        # retain the isolated operator fixture for this negative path.
        browser_host=None,
    )
    failure_context = failure_chrome.isolated_context()
    wait_until(
        lambda: failure_chrome.evaluate(
            "globalThis.__ATO_BROWSER_LIFECYCLE__", failure_context
        )
        == "restoring"
    )
    failure_process.wait(timeout=FAILURE_TIMEOUT_SECONDS)
    failure_stdout_handle.close()
    failure_stderr_handle.close()
    failure_stdout = failure_stdout_path.read_text()
    failure_stderr = failure_stderr_path.read_text()
    atexit.unregister(stop_child)
    failure_chrome.close()
    if failure_process.returncode == 0:
        raise AcceptanceError("Bridge disconnect was reported as successful Replay")
    if "Browser Bridge disconnected" not in failure_stderr:
        raise AcceptanceError(
            f"Bridge disconnect failure was not explicit: {failure_stderr}"
        )
    wait_until(lambda: not list(failure_runtime.glob("browser-*.json")))
    wait_until(lambda: server_is_closed(port))
    receipt = {
        "schema": "ato.browser.staging-acceptance/v1",
        "result": "PASS",
        "commit": subprocess.run(
            ["git", "-C", str(args.repository), "rev-parse", "HEAD"],
            check=True,
            text=True,
            capture_output=True,
        ).stdout.strip(),
        "runner": socket.gethostname(),
        "chrome": recipient_chrome.version,
        "browser_host": args.browser_host,
        "origin": origin,
        "initial_computation": initial_head,
        "sealed_computation": final_head,
        "portable_root": portable["index"]["root"],
        "recipient_run": recipient_project.parent.name,
        "browser_records": len(browser_records),
        "replay_applied_records": len(records),
        "verification": {
            "kind": "independent_http_and_dom_contract",
            "state_after_replay": 2,
            "state_after_continue": final_state,
        },
        "continuation": {
            "computation": continued_head,
            "browser_records": len(continued_records),
            "portable_root": continued_portable["index"]["root"],
            "sealable": True,
        },
        "browser_http": {
            "policy": "fail_closed_before_replay",
            "recorded_boundaries": sorted(mixed_adapters),
            "state_before_replay_rejection": mixed_state,
            "encap_exit_code": mixed_encap.returncode,
            "explicit_diagnostic": mixed_diagnostic in mixed_encap.stderr,
            "double_effect": "absent",
        },
        "security": {
            "credential_absent_from_bundle": True,
            "credential_absent_from_page_realm": True,
            "runtime_discovery_cleanup": True,
            "real_input_during_replay_blocked": not args.browser_host,
            "replay_input_blocking_coverage": (
                "default operator-injection mode"
                if args.browser_host
                else "this run"
            ),
            "fresh_chrome_process": True,
            "browser_http_double_effect": "absent; mixed descriptors fail closed",
        },
        "failure_path": {
            "kind": "bridge_disconnect_during_replay",
            "exit_code": failure_process.returncode,
            "explicit_error": "Browser Bridge disconnected" in failure_stderr,
            "runtime_discovery_cleanup": True,
            "workload_process_cleanup": True,
            "stdout": failure_stdout,
            "stderr": failure_stderr,
        },
        "portable_stderr": stderr,
        "portable_stdout": stdout,
    }
    args.receipt.parent.mkdir(parents=True, exist_ok=True)
    args.receipt.write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt, indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        print(f"staging Chrome acceptance failed: {error}", file=sys.stderr)
        raise
