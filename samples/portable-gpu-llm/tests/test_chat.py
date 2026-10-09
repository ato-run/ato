import importlib.util
import json
import sys
import socketserver
import tempfile
import threading
import time
import unittest
from unittest import mock
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import urlopen

sys.path.insert(0, str(Path(__file__).parents[1]))

spec = importlib.util.spec_from_file_location("gpu_chat", Path(__file__).parents[1] / "app.py")
app = importlib.util.module_from_spec(spec)
spec.loader.exec_module(app)


class EngineServer(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True


class ChatTests(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.TemporaryDirectory()
        self.addCleanup(self.root.cleanup)
        self.path = Path(self.root.name) / "engine.sock"
        self.requests = []
        self.started = threading.Event()
        self.disconnected = threading.Event()
        self.hold = False
        owner = self

        class Engine(BaseHTTPRequestHandler):
            def do_POST(self):
                owner.requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                self.wfile.write(b'data: {"choices":[{"delta":{"content":"hello"}}]}\n\n')
                self.wfile.flush()
                owner.started.set()
                if owner.hold:
                    self.connection.settimeout(3)
                    if self.connection.recv(1) == b"":
                        owner.disconnected.set()
                else:
                    self.wfile.write(b'data: {"choices":[{"delta":{"content":" GPU"}}]}\n\ndata: [DONE]\n\n')

            def log_message(self, *_):
                pass

        self.engine = EngineServer(str(self.path), Engine)
        threading.Thread(target=self.engine.serve_forever, daemon=True).start()
        self.addCleanup(self.engine.server_close)
        self.addCleanup(self.engine.shutdown)
        self.chat = app.Chat(Path(self.root.name) / "out", self.path)
        self.chat.phase = "ready"

    def completed(self, request_id):
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            value = self.chat.snapshot(request_id)
            if value["phase"] in app.TERMINAL:
                return value
            time.sleep(0.01)
        self.fail("generation did not finish")

    def test_streamed_result_is_checkpointed_and_duplicate_submission_never_regenerates(self):
        request_id = str(uuid.uuid4())
        self.chat.start(request_id, "hello")
        value = self.completed(request_id)
        self.assertEqual((value["phase"], value["text"]), ("completed", "hello GPU"))
        self.assertGreater(value["sequence"], 1)
        existing, created = self.chat.start(request_id, "hello")
        self.assertFalse(created)
        self.assertEqual(existing, value)
        with self.assertRaisesRegex(ValueError, "request_id_conflict"):
            self.chat.start(request_id, "different")
        self.assertEqual(len(self.requests), 1)
        restored = app.Chat(Path(self.root.name) / "restored", self.path, json.loads((self.chat.output / "chat-history.json").read_text()))
        self.assertEqual(restored.snapshot(request_id)["text"], "hello GPU")
        self.assertFalse(restored.start(request_id, "hello")[1])
        self.assertEqual(len(self.requests), 1)

    def test_cancel_disconnects_inference_and_retains_partial_text(self):
        self.hold = True
        request_id = str(uuid.uuid4())
        self.chat.start(request_id, "cancel me")
        self.assertTrue(self.started.wait(2))
        deadline = time.monotonic() + 2
        while not self.chat.snapshot(request_id)["text"] and time.monotonic() < deadline:
            time.sleep(0.01)
        with self.assertRaisesRegex(ValueError, "generation_unavailable"):
            self.chat.start(str(uuid.uuid4()), "second")
        self.chat.cancel(request_id)
        value = self.completed(request_id)
        self.assertEqual(value["phase"], "cancelled")
        self.assertEqual(value["text"], "hello")
        self.assertTrue(self.disconnected.wait(2))
        self.assertIsNone(self.chat.active)
        self.assertEqual(json.loads((self.chat.output / "chat-history.json").read_text())["jobs"][0]["phase"], "cancelled")

    def test_restart_marks_incomplete_generation_interrupted_without_execution(self):
        raw = {"schema": "ato.gpu-chat-history/1", "jobs": [{"request_id": str(uuid.uuid4()), "prompt": "pending", "text": "part",
                "phase": "generating", "error": None, "created_at": "2026-10-08T00:00:00+00:00", "sequence": 4}]}
        restored = app.Chat(Path(self.root.name) / "restored", self.path, raw)
        self.assertEqual(restored.jobs[0]["phase"], "interrupted")
        self.assertEqual(restored.jobs[0]["text"], "part")
        self.assertEqual(self.requests, [])

    def test_input_and_history_bounds_refuse_before_inference(self):
        for prompt in ["", "x" * 4097, None]:
            with self.assertRaises(ValueError):
                self.chat.start(str(uuid.uuid4()), prompt)
        with self.assertRaises(ValueError):
            app.Chat(Path(self.root.name) / "restored", self.path, {"schema": "unknown", "jobs": []})
        self.assertEqual(self.requests, [])

    def test_checkpoint_failure_does_not_start_unrecorded_inference(self):
        with mock.patch.object(self.chat, "persist", side_effect=OSError("disk full")):
            with self.assertRaisesRegex(ValueError, "history_save_failed"):
                self.chat.start(str(uuid.uuid4()), "must be recorded first")
        self.assertEqual(self.chat.jobs, [])
        self.assertIsNone(self.chat.active)
        self.assertEqual(self.requests, [])

    def test_readiness_is_not_satisfied_by_html_or_an_engine_fixture(self):
        self.chat.phase = "preparing"
        server = ThreadingHTTPServer(("127.0.0.1", 0), app.handler(self.chat))
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        base = f"http://127.0.0.1:{server.server_port}"
        with self.assertRaises(HTTPError) as response:
            urlopen(base + "/health")
        self.assertEqual(response.exception.code, 503)
        response.exception.close()
        with urlopen(base + "/") as html:
            self.assertEqual(html.status, 200)
        self.assertEqual(self.requests, [])


if __name__ == "__main__":
    unittest.main()
