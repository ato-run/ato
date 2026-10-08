"""One fixed, offline GPU chat route. Provider operations belong to the Runner."""
import http.client
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import tarfile
import threading
import time
import uuid
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TERMINAL = {"completed", "cancelled", "failed", "interrupted"}
MAX_HISTORY_BYTES = 1024 * 1024
MAX_JOBS = 64
MAX_TEXT = 32768


class UnixConnection(http.client.HTTPConnection):
    def __init__(self, path):
        super().__init__("localhost", timeout=120)
        self.path = str(path)

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self.path)


def validate_history(raw):
    if not isinstance(raw, dict) or set(raw) != {"schema", "jobs"}:
        raise ValueError("invalid_history")
    if raw["schema"] != "ato.gpu-chat-history/1" or not isinstance(raw["jobs"], list) or len(raw["jobs"]) > MAX_JOBS:
        raise ValueError("invalid_history")
    ids = set()
    for job in raw["jobs"]:
        if not isinstance(job, dict) or set(job) != {"request_id", "prompt", "text", "phase", "error", "created_at", "sequence"}:
            raise ValueError("invalid_history")
        if not isinstance(job["request_id"], str) or str(uuid.UUID(job["request_id"])) != job["request_id"] or job["request_id"] in ids:
            raise ValueError("invalid_history")
        ids.add(job["request_id"])
        if not isinstance(job["prompt"], str) or not 1 <= len(job["prompt"]) <= 4096 or not isinstance(job["text"], str) or len(job["text"]) > MAX_TEXT:
            raise ValueError("invalid_history")
        if job["phase"] not in TERMINAL | {"generating", "cancelling"}:
            raise ValueError("invalid_history")
        if job["error"] is not None and (not isinstance(job["error"], str) or len(job["error"]) > 500):
            raise ValueError("invalid_history")
        if type(job["sequence"]) is not int or job["sequence"] < 0 or not isinstance(job["created_at"], str):
            raise ValueError("invalid_history")
        datetime.fromisoformat(job["created_at"])
    if len(json.dumps(raw, ensure_ascii=False).encode()) > MAX_HISTORY_BYTES:
        raise ValueError("history_capacity_exceeded")
    return raw["jobs"]


class Chat:
    def __init__(self, output, socket_path, restored=None):
        self.output = Path(output)
        self.output.mkdir(parents=True, exist_ok=True)
        self.socket_path = socket_path
        self.lock = threading.RLock()
        self.jobs = validate_history(restored) if restored is not None else []
        self.active = None
        self.phase = "preparing"
        self.error = None
        for job in self.jobs:
            if job["phase"] not in TERMINAL:
                job.update(phase="interrupted", error="Previous Run stopped during generation", sequence=job["sequence"] + 1)
        self.persist()

    def persist(self):
        raw = json.dumps({"schema": "ato.gpu-chat-history/1", "jobs": self.jobs}, ensure_ascii=False).encode()
        if len(raw) > MAX_HISTORY_BYTES:
            raise ValueError("history_capacity_exceeded")
        temporary = self.output / ".chat-history.partial"
        with temporary.open("wb") as handle:
            handle.write(raw)
            handle.flush()
            os.fsync(handle.fileno())
        temporary.replace(self.output / "chat-history.json")

    def history(self):
        with self.lock:
            return json.loads(json.dumps({"schema": "ato.gpu-chat-history/1", "jobs": self.jobs}))

    def start(self, request_id, prompt):
        if not isinstance(request_id, str) or str(uuid.UUID(request_id)) != request_id:
            raise ValueError("invalid_request_id")
        if not isinstance(prompt, str) or not 1 <= len(prompt.strip()) <= 4096:
            raise ValueError("invalid_prompt")
        prompt = prompt.strip()
        with self.lock:
            previous = next((job for job in self.jobs if job["request_id"] == request_id), None)
            if previous:
                if previous["prompt"] != prompt:
                    raise ValueError("request_id_conflict")
                return dict(previous), False
            if self.phase != "ready" or self.active is not None:
                raise ValueError("generation_unavailable")
            if len(self.jobs) >= MAX_JOBS or len(json.dumps(self.history()).encode()) > MAX_HISTORY_BYTES - 150000:
                raise ValueError("history_capacity_exceeded")
            messages = []
            for job in [job for job in self.jobs if job["phase"] == "completed"][-3:]:
                messages.extend([{"role": "user", "content": job["prompt"]}, {"role": "assistant", "content": job["text"]}])
            messages.append({"role": "user", "content": prompt})
            job = {"request_id": request_id, "prompt": prompt, "text": "", "phase": "generating", "error": None,
                   "created_at": datetime.now(timezone.utc).isoformat(), "sequence": 0}
            self.jobs.append(job)
            try:
                self.persist()
            except OSError as error:
                self.jobs.pop()
                raise ValueError("history_save_failed") from error
            active = {"job": job, "cancel": threading.Event(), "socket": None}
            self.active = active
            threading.Thread(target=self.generate, args=(active, messages), daemon=True).start()
            return dict(job), True

    def generate(self, active, messages):
        job = active["job"]
        connection = UnixConnection(self.socket_path)
        response = None
        cancel_socket = None
        try:
            if active["cancel"].is_set():
                return
            connection.request("POST", "/v1/chat/completions", json.dumps({"messages": messages, "stream": True, "max_tokens": 512,
                                                                         "temperature": 0.2}), {"Content-Type": "application/json"})
            # HTTP/1.0 may transfer socket ownership to HTTPResponse. Keep an
            # independent handle to interrupt its blocking read on cancellation.
            cancel_socket = connection.sock.dup()
            with self.lock:
                active["socket"] = cancel_socket
                if active["cancel"].is_set():
                    cancel_socket.shutdown(socket.SHUT_RDWR)
            response = connection.getresponse()
            if response.status != 200:
                raise RuntimeError(f"Inference refused ({response.status})")
            checkpoint = time.monotonic()
            done = False
            while not active["cancel"].is_set():
                line = response.readline(1024 * 1024)
                if not line:
                    break
                if not line.startswith(b"data: "):
                    continue
                if line.strip() == b"data: [DONE]":
                    done = True
                    break
                value = json.loads(line[6:])
                if "error" in value:
                    raise RuntimeError("Inference returned an error")
                choices = value.get("choices", [])
                text = choices[0].get("delta", {}).get("content", "") if choices else ""
                if not isinstance(text, str):
                    raise RuntimeError("Invalid inference stream")
                with self.lock:
                    if len(job["text"]) + len(text) > MAX_TEXT:
                        raise RuntimeError("Response limit exceeded")
                    job["text"] += text
                    job["sequence"] += 1
                    if time.monotonic() - checkpoint >= 1:
                        self.persist()
                        checkpoint = time.monotonic()
            if not done and not active["cancel"].is_set():
                raise RuntimeError("Inference stream ended before completion")
            with self.lock:
                job["phase"] = "completed"
        except Exception as error:
            with self.lock:
                job.update(phase="failed", error=str(error)[:500])
        finally:
            if response is not None:
                response.close()
            if cancel_socket is not None:
                cancel_socket.close()
            connection.close()
            with self.lock:
                if active["cancel"].is_set():
                    job.update(phase="cancelled", error=None)
                job["sequence"] += 1
                self.active = None
                try:
                    self.persist()
                except OSError:
                    job.update(phase="failed", error="history_save_failed")

    def cancel(self, request_id):
        with self.lock:
            if self.active is None or self.active["job"]["request_id"] != request_id:
                return
            active = self.active
            active["cancel"].set()
            active["job"].update(phase="cancelling", sequence=active["job"]["sequence"] + 1)
            try:
                self.persist()
            except OSError:
                active["job"]["error"] = "history_save_failed"
            cancel_socket = active["socket"]
            if cancel_socket is not None:
                try:
                    cancel_socket.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass

    def snapshot(self, request_id):
        with self.lock:
            return next((dict(job) for job in self.jobs if job["request_id"] == request_id), None)


def prepare(chat, scratch, software, models):
    try:
        scratch.mkdir(parents=True, exist_ok=True, mode=0o700)
        for name in ["llama-cuda.tar.gz", "cuda-runtime.tar.gz"]:
            with tarfile.open(software / name) as package:
                package.extractall(scratch, filter="data")
        engines = list(scratch.rglob("llama-server"))
        if len(engines) != 1:
            raise RuntimeError("Pinned engine archive must contain one llama-server")
        engine = engines[0]
        libraries = sorted({str(path.parent) for path in scratch.rglob("*.so*")})
        environment = dict(os.environ, LD_LIBRARY_PATH=":".join(libraries), HF_HUB_OFFLINE="1")
        devices = subprocess.run([str(engine), "--list-devices"], env=environment, capture_output=True, text=True, check=True, timeout=30)
        if not re.search(r"(?m)^\s*CUDA0:", devices.stdout + devices.stderr):
            raise RuntimeError("CUDA GPU is unavailable; CPU fallback is refused")
        socket_path = Path(chat.socket_path)
        if len(os.fsencode(socket_path)) > 100:
            raise RuntimeError("Private inference socket path exceeds the Unix socket limit")
        log_path = scratch / "engine.log"
        with log_path.open("wb") as log:
            child = subprocess.Popen([str(engine), "--model", str(models / "qwen.gguf"), "--offline", "--device", "CUDA0",
                                      "--gpu-layers", "all", "--fit", "off", "--split-mode", "none", "--ctx-size", "4096",
                                      "--parallel", "1", "--host", str(socket_path), "--no-webui"],
                                     env=environment, stdout=log, stderr=log)
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            if child.poll() is not None:
                raise RuntimeError(f"GPU engine exited ({child.returncode})")
            try:
                connection = UnixConnection(socket_path)
                connection.request("GET", "/health")
                response = connection.getresponse()
                healthy = response.status == 200
                response.read()
            except OSError:
                healthy = False
            finally:
                connection.close()
            offload = re.search(r"offloaded (\d+)/(\d+) layers to GPU", log_path.read_text(errors="replace"))
            if healthy and offload and int(offload[1]) > 0 and offload[1] == offload[2]:
                with chat.lock:
                    chat.phase = "ready"
                child.wait()
                raise RuntimeError(f"GPU engine exited ({child.returncode})")
            time.sleep(0.2)
        child.terminate()
        try:
            child.wait(timeout=10)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()
        raise RuntimeError("GPU load deadline or complete layer-offload evidence was not satisfied")
    except Exception as error:
        with chat.lock:
            chat.phase, chat.error = "failed", str(error)[:500]


def handler(chat):
    class Handler(BaseHTTPRequestHandler):
        def send_bytes(self, status, body, content_type):
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)

        def send_json(self, status, value):
            self.send_bytes(status, json.dumps(value).encode(), "application/json")

        def do_GET(self):
            if self.path == "/":
                self.send_bytes(200, Path(__file__).with_name("index.html").read_bytes(), "text/html; charset=utf-8")
            elif self.path == "/health":
                with chat.lock:
                    self.send_bytes(200 if chat.phase == "ready" else 503, b"gpu-llm-ready\n" if chat.phase == "ready" else b"not-ready\n", "text/plain")
            elif self.path == "/api/status":
                with chat.lock:
                    self.send_json(200, {"phase": chat.phase, "error": chat.error})
            elif self.path == "/api/history":
                self.send_json(200, chat.history())
            elif self.path.startswith("/api/events/"):
                request_id = self.path.removeprefix("/api/events/")
                if chat.snapshot(request_id) is None:
                    self.send_json(404, {"error": "not_found"})
                    return
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Cache-Control", "no-store")
                self.send_header("Connection", "close")
                self.end_headers()
                sequence = -1
                deadline = time.monotonic() + 30
                try:
                    while time.monotonic() < deadline:
                        job = chat.snapshot(request_id)
                        if job["sequence"] != sequence:
                            self.wfile.write(b"data: " + json.dumps(job).encode() + b"\n\n")
                            self.wfile.flush()
                            sequence = job["sequence"]
                        if job["phase"] in TERMINAL:
                            return
                        time.sleep(0.1)
                except (BrokenPipeError, ConnectionResetError):
                    pass
                self.close_connection = True
            else:
                self.send_json(404, {"error": "not_found"})

        def do_POST(self):
            try:
                if self.headers.get("Content-Type", "").split(";")[0] != "application/json":
                    raise ValueError("json_required")
                length = int(self.headers.get("Content-Length", "-1"))
                if not 0 <= length <= 20000:
                    raise ValueError("invalid_length")
                value = json.loads(self.rfile.read(length))
                if self.path == "/api/chat" and isinstance(value, dict) and set(value) == {"request_id", "prompt"}:
                    job, created = chat.start(value["request_id"], value["prompt"])
                    self.send_json(202 if created else 200, job)
                elif self.path == "/api/cancel" and isinstance(value, dict) and set(value) == {"request_id"} and isinstance(value["request_id"], str):
                    chat.cancel(value["request_id"])
                    self.send_json(202, {"ok": True})
                else:
                    raise ValueError("invalid_request")
            except (ValueError, TypeError, KeyError, AttributeError) as error:
                self.send_json(409, {"error": str(error)[:200]})

        def log_message(self, *_):
            pass
    return Handler


def main():
    scratch = Path(os.environ["TMPDIR"]) / "gpu-llm"
    socket_path = Path(os.environ["TMPDIR"]) / "llm.sock"
    assets = Path(os.environ.get("ATO_INPUT_ASSETS_DIR", scratch / "no-assets"))
    restored = list(assets.glob("*/chat-history.json"))
    if len(restored) > 1:
        raise ValueError("Select exactly one saved chat-history input")
    if restored and restored[0].stat().st_size > MAX_HISTORY_BYTES:
        raise ValueError("history_capacity_exceeded")
    history = json.loads(restored[0].read_text()) if restored else None
    chat = Chat(os.environ["ATO_OUTPUT_DIR"], socket_path, history)
    server = ThreadingHTTPServer((os.environ.get("APP_LISTEN_HOST", "127.0.0.1"), int(os.environ["ATO_ENDPOINT_APP_HTTP_PORT"])), handler(chat))
    threading.Thread(target=prepare, args=(chat, scratch, Path(os.environ["ATO_INPUT_PATH_SOFTWARE"]), Path(os.environ["ATO_INPUT_PATH_MODELS"])), daemon=True).start()
    server.serve_forever()


if __name__ == "__main__":
    main()
