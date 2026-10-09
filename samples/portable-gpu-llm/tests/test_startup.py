"""Real CPU fixture processes/sockets; CUDA enumeration/offload text is simulated."""
import importlib.util
import json
import os
from pathlib import Path
import sys
import socket
import subprocess
import tarfile
import tempfile
import threading
import time
import unittest
from http.server import ThreadingHTTPServer
from urllib.error import HTTPError
from urllib.request import urlopen

sys.path.insert(0, str(Path(__file__).parents[1]))
import app
from engine_startup import EngineStartup, LOG_LIMIT


class StartupTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    def fixture(self, mode, timeout=2):
        source = self.root / 'fixture'
        source.mkdir()
        engine = source / 'llama-server'
        engine.write_text(f'''#!{sys.executable}
import os, sys, time, socketserver
from http.server import BaseHTTPRequestHandler
if '--help' in sys.argv: print('fixture help'); sys.exit(0)
if '--list-devices' in sys.argv: print('CUDA0: fixture (simulated)'); sys.exit(0)
print('fixture model load', flush=True)
print('fixture stderr diagnostic', file=sys.stderr, flush=True)
if {mode!r} == 'exit': sys.exit(7)
if {mode!r} == 'flood':
    print('x' * (300 * 1024), flush=True)
if {mode!r} in ('hang', 'flood'):
    while True: time.sleep(1)
path = sys.argv[sys.argv.index('--host') + 1]
class Health(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200); self.end_headers()
    def log_message(self, *_): pass
class Server(socketserver.UnixStreamServer): pass
time.sleep(0.25)
print('offloaded 29/29 layers to GPU', file=sys.stderr, flush=True)
with Server(path, Health) as server: server.serve_forever()
''')
        engine.chmod(0o755)
        software = self.root / 'software'
        software.mkdir()
        with tarfile.open(software / 'llama-cuda.tar.gz', 'w:gz') as archive:
            archive.add(engine, arcname='llama-server')
        with tarfile.open(software / 'cuda-runtime.tar.gz', 'w:gz'):
            pass
        self.chat = app.Chat(self.root / 'outputs', self.root / 'engine.sock')
        self.startup = EngineStartup(self.chat, self.root / 'scratch', software, self.root / 'models', app.engine_health,
                                    timeout, abi_check=lambda *_: {'fixture': True})
        self.chat.startup = self.startup
        self.thread = threading.Thread(target=self.startup.run)
        self.thread.start()
        self.addCleanup(self.cleanup_engine)
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), app.handler(self.chat))
        self.server.daemon_threads = True
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.url = f'http://127.0.0.1:{self.server.server_port}/health'

    def cleanup_engine(self):
        self.startup.stop.set()
        self.thread.join(3)
        self.assertFalse(self.thread.is_alive())
        if self.startup.child:
            self.assertIsNotNone(self.startup.child.poll(), 'engine is reaped before evidence is final')

    def diagnostic(self):
        return json.loads((self.chat.output / 'startup-diagnostics.json').read_text())

    def test_preparing_503_then_ready_200_with_full_offload_evidence(self):
        self.fixture('ready')
        with self.assertRaises(HTTPError) as response:
            urlopen(self.url, timeout=1)
        self.assertEqual(response.exception.code, 503)
        response.exception.close()
        deadline = time.monotonic() + 3
        while self.chat.phase == 'preparing' and time.monotonic() < deadline:
            time.sleep(0.01)
        with urlopen(self.url, timeout=1) as response:
            self.assertEqual(response.status, 200)
            self.assertEqual(response.read(), b'gpu-llm-ready\n')
        self.assertEqual(self.diagnostic()['offload_layers'], [29, 29])
        self.assertTrue(self.diagnostic()['cuda_recognized'])
        self.cleanup_engine()
        self.assertEqual(self.diagnostic()['stage'], 'stopped')

    def test_engine_exit_preserves_exit_code_and_both_logs(self):
        self.fixture('exit')
        self.assertTrue(self.startup.finished.wait(3))
        self.assertEqual(self.chat.phase, 'failed')
        self.assertEqual(self.diagnostic()['engine_exit_code'], 7)
        self.assertEqual(self.diagnostic()['error'], 'engine_exited:7')
        self.assertEqual([phase['stage'] for phase in self.diagnostic()['phases']], ['created', 'extracting', 'abi_check', 'cuda_check', 'model_loading', 'failed'])
        self.assertIn('fixture model load', (self.chat.output / 'engine-stdout.log').read_text())
        self.assertIn('stderr diagnostic', (self.chat.output / 'engine-stderr.log').read_text())

    def test_timeout_reaps_engine_and_keeps_bounded_diagnostics(self):
        self.fixture('flood', timeout=3)
        self.assertTrue(self.startup.finished.wait(6))
        self.assertTrue(self.diagnostic()['engine_started'])
        self.assertEqual(self.diagnostic()['error'], 'startup_timeout')
        self.assertIsNotNone(self.diagnostic()['engine_exit_code'])
        self.assertLessEqual((self.chat.output / 'engine-stdout.log').stat().st_size, LOG_LIMIT)
        self.assertGreater(self.diagnostic()['log_bytes_seen']['stdout'], LOG_LIMIT)
        self.assertLess((self.chat.output / 'startup-diagnostics.json').stat().st_size, 32768)

    def test_cancel_during_preparation_reaps_and_saves_empty_history_and_diagnostics(self):
        self.fixture('hang')
        deadline = time.monotonic() + 2
        while not self.diagnostic()['engine_started'] and time.monotonic() < deadline:
            time.sleep(0.01)
        self.assertTrue(self.diagnostic()['engine_started'])
        self.startup.stop.set()
        self.assertTrue(self.startup.finished.wait(3))
        self.cleanup_engine()
        self.assertEqual(self.diagnostic()['stage'], 'cancelled')
        self.assertIsNotNone(self.diagnostic()['engine_exit_code'])
        self.assertEqual(json.loads((self.chat.output / 'chat-history.json').read_text())['jobs'], [])

    def test_incompatible_abi_fails_before_cuda_or_engine(self):
        self.fixture('ready')
        # Independent Run so the rejection cannot race the successful fixture.
        chat = app.Chat(self.root / 'abi-out', self.root / 'abi.sock')
        def refused(*_):
            raise RuntimeError('process_abi_incompatible: glibc=2.35, GLIBCXX=3.4.30')
        manager = EngineStartup(chat, self.root / 'abi-scratch', self.root / 'software', self.root / 'models', app.engine_health, abi_check=refused)
        manager.run()
        diagnostic = json.loads((chat.output / 'startup-diagnostics.json').read_text())
        self.assertEqual(chat.phase, 'failed')
        self.assertFalse(diagnostic['engine_started'])
        self.assertIsNone(diagnostic['cuda_probe'])
        self.assertIsNone(manager.child)

    def run_main(self, mode, cancel):
        self.fixture(mode)
        self.cleanup_engine()
        with socket.socket() as reservation:
            reservation.bind(('127.0.0.1', 0))
            port = reservation.getsockname()[1]
        outputs = self.root / 'main-outputs'
        environment = dict(os.environ, TMPDIR=str(self.root), ATO_OUTPUT_DIR=str(outputs),
                           ATO_ENDPOINT_APP_HTTP_PORT=str(port), ATO_INPUT_PATH_SOFTWARE=str(self.root / 'software'),
                           ATO_INPUT_PATH_MODELS=str(self.root / 'models'))
        script = "import app; from engine_startup import EngineStartup; app.EngineStartup=lambda *args: EngineStartup(*args, abi_check=lambda *_: {'fixture': True}); app.main()"
        child = subprocess.Popen([sys.executable, '-B', '-c', script], cwd=Path(__file__).parents[1], env=environment,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            if cancel:
                deadline = time.monotonic() + 4
                while time.monotonic() < deadline:
                    diagnostic_path = outputs / 'startup-diagnostics.json'
                    if diagnostic_path.exists() and json.loads(diagnostic_path.read_text())['engine_started']:
                        break
                    time.sleep(0.01)
                self.assertIsNone(child.poll())
                self.assertTrue(json.loads(diagnostic_path.read_text())['engine_started'])
                child.terminate()
            stdout, stderr = child.communicate(timeout=4)
            self.assertEqual(child.returncode, 0 if cancel else 1, (stdout, stderr))
            diagnostic = json.loads((outputs / 'startup-diagnostics.json').read_text())
            self.assertEqual(diagnostic['stage'], 'cancelled' if cancel else 'failed')
            self.assertIsNotNone(diagnostic['engine_exit_code'])
            with self.assertRaises(ProcessLookupError):
                os.kill(diagnostic['engine_pid'], 0)
            self.assertEqual(json.loads((outputs / 'chat-history.json').read_text())['jobs'], [])
        finally:
            if child.poll() is None:
                child.kill()
            child.communicate(timeout=3)

    def test_main_exits_nonzero_after_engine_failure_and_saves_diagnostics(self):
        self.run_main('exit', cancel=False)

    def test_sigterm_during_preparation_stops_main_and_engine_after_diagnostic_save(self):
        self.run_main('hang', cancel=True)


if __name__ == '__main__':
    unittest.main()
