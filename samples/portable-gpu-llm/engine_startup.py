"""Bounded engine preparation and durable diagnostics. No provider operations."""
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import threading
import time

LOG_LIMIT = 256 * 1024


def atomic(path, raw):
    temporary = path.with_name('.' + path.name + '.partial')
    with temporary.open('wb') as handle:
        handle.write(raw)
        handle.flush()
        os.fsync(handle.fileno())
    temporary.replace(path)


def measured_abi(engine, environment):
    glibc = os.confstr('CS_GNU_LIBC_VERSION').removeprefix('glibc ')
    triplet = 'x86_64-linux-gnu' if os.uname().machine == 'x86_64' else 'aarch64-linux-gnu'
    paths = [Path(p) / 'libstdc++.so.6' for p in environment['LD_LIBRARY_PATH'].split(':') if p]
    paths.append(Path('/usr/lib') / triplet / 'libstdc++.so.6')
    library = next((p for p in paths if p.is_file()), None)
    if library is None or library.stat().st_size > 64 * 1024 * 1024:
        raise RuntimeError('process_abi_unknown: libstdc++')
    versions = [part.removeprefix(b'GLIBCXX_') for part in library.read_bytes().split(b'\x00') if re.fullmatch(rb'GLIBCXX_\d+\.\d+(?:\.\d+)?', part)]
    numeric = lambda v: tuple(int(p) for p in v.split('.'))
    glibcxx = max((v.decode() for v in versions), key=numeric, default='0.0.0')
    if numeric(glibc) < (2, 38) or numeric(glibcxx) < (3, 4, 32):
        raise RuntimeError(f'process_abi_incompatible: glibc={glibc}, GLIBCXX={glibcxx}')
    return {'glibc': glibc, 'glibcxx': glibcxx, 'library': str(library)}


class EngineStartup:
    def __init__(self, chat, scratch, software, models, health, timeout=180, abi_check=measured_abi):
        self.chat, self.scratch, self.software, self.models = chat, Path(scratch), Path(software), Path(models)
        self.health, self.timeout, self.abi_check = health, timeout, abi_check
        self.stop = threading.Event()
        self.finished = threading.Event()
        self.lock = threading.RLock()
        self.child = None
        self.tail = {'stdout': b'', 'stderr': b''}
        self.log_errors = []
        self.started = time.monotonic()
        self.diagnostic = {'schema': 'ato.gpu-engine-startup/1', 'stage': 'created', 'error': None,
                           'cuda_recognized': None, 'cuda_probe': None, 'abi': None,
                           'engine_exit_code': None, 'engine_started': False,
                           'engine_http_status': None, 'offload_layers': None,
                           'elapsed_ms': 0, 'phases': [], 'log_limit_bytes_each': LOG_LIMIT, 'log_bytes_seen': {'stdout': 0, 'stderr': 0}}
        self.record()

    def record(self, **updates):
        with self.lock:
            self.diagnostic.update(updates)
            self.diagnostic['elapsed_ms'] = round((time.monotonic() - self.started) * 1000)
            phases = self.diagnostic['phases']
            if not phases or phases[-1]['stage'] != self.diagnostic['stage']:
                phases.append({'stage': self.diagnostic['stage'], 'elapsed_ms': self.diagnostic['elapsed_ms']})
                del phases[:-16]
            atomic(self.chat.output / 'startup-diagnostics.json', json.dumps(self.diagnostic).encode())

    def status(self):
        with self.lock:
            return {k: self.diagnostic[k] for k in ['stage', 'cuda_recognized', 'engine_exit_code', 'engine_http_status', 'offload_layers', 'abi']}

    def remaining(self):
        if self.stop.is_set():
            raise RuntimeError('startup_cancelled')
        remaining = self.started + self.timeout - time.monotonic()
        if remaining <= 0:
            raise RuntimeError('startup_timeout')
        return remaining

    def collect(self, name, pipe, persist):
        last_write = 0
        try:
            while True:
                chunk = pipe.read1(4096)
                if not chunk:
                    break
                with self.lock:
                    self.tail[name] = (self.tail[name] + chunk)[-LOG_LIMIT:]
                    if persist:
                        self.diagnostic['log_bytes_seen'][name] += len(chunk)
                        if time.monotonic() - last_write >= 0.1:
                            atomic(self.chat.output / ('engine-' + name + '.log'), self.tail[name])
                            last_write = time.monotonic()
            if persist and self.tail[name]:
                atomic(self.chat.output / ('engine-' + name + '.log'), self.tail[name])
        except Exception as error:
            with self.lock:
                self.log_errors.append(f'{name}:{type(error).__name__}')
        finally:
            pipe.close()

    def spawn(self, argv, environment, persist):
        self.child = subprocess.Popen(argv, env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.tail = {'stdout': b'', 'stderr': b''}
        threads = [threading.Thread(target=self.collect, args=(name, pipe, persist))
                   for name, pipe in [('stdout', self.child.stdout), ('stderr', self.child.stderr)]]
        for thread in threads:
            thread.start()
        return self.child, threads

    def stop_child(self):
        child = self.child
        if child is None:
            return None
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=1)
            except subprocess.TimeoutExpired:
                child.kill()
        return child.wait(timeout=1)

    def probe_command(self, argv, environment):
        deadline = time.monotonic() + min(30, self.remaining())
        child, threads = self.spawn(argv, environment, False)
        try:
            while child.poll() is None:
                self.remaining()
                if time.monotonic() >= deadline:
                    raise RuntimeError('engine_probe_timeout')
                self.stop.wait(0.02)
            for thread in threads:
                thread.join(timeout=1)
            if any(thread.is_alive() for thread in threads) or self.log_errors:
                raise RuntimeError('engine_probe_capture_failed')
            return {'exit_code': child.returncode, 'stdout': self.tail['stdout'].decode(errors='replace')[-2048:],
                    'stderr': self.tail['stderr'].decode(errors='replace')[-2048:]}
        finally:
            self.stop_child()
            for thread in threads:
                thread.join(timeout=1)

    def run(self):
        threads = []
        engine_child = None
        try:
            self.record(stage='extracting')
            self.scratch.mkdir(parents=True, exist_ok=True, mode=0o700)
            for name in ['llama-cuda.tar.gz', 'cuda-runtime.tar.gz']:
                self.remaining()
                with tarfile.open(self.software / name) as package:
                    package.extractall(self.scratch, filter='data')
            engines = list(self.scratch.rglob('llama-server'))
            if len(engines) != 1:
                raise RuntimeError('Pinned engine archive must contain one llama-server')
            engine = engines[0]
            libraries = sorted({str(p.parent) for p in self.scratch.rglob('*.so*')})
            environment = dict(os.environ, LD_LIBRARY_PATH=':'.join(libraries), HF_HUB_OFFLINE='1')
            self.record(stage='abi_check')
            self.record(abi=self.abi_check(engine, environment))
            loader = self.probe_command([str(engine), '--help'], environment)
            self.record(loader_probe=loader)
            if loader['exit_code'] != 0:
                raise RuntimeError('engine_loader_failed')
            self.record(stage='cuda_check')
            devices = self.probe_command([str(engine), '--list-devices'], environment)
            cuda = bool(re.search(r'(?m)^\s*CUDA0:', devices['stdout'] + devices['stderr']))
            self.record(cuda_probe=devices, cuda_recognized=cuda)
            if devices['exit_code'] != 0 or not cuda:
                raise RuntimeError('CUDA GPU is unavailable; CPU fallback is refused')
            if len(os.fsencode(self.chat.socket_path)) > 100:
                raise RuntimeError('Private inference socket path exceeds the Unix socket limit')
            self.remaining()
            engine_child, threads = self.spawn([str(engine), '--model', str(self.models / 'qwen.gguf'), '--offline', '--device', 'CUDA0',
                                               '--gpu-layers', 'all', '--fit', 'off', '--split-mode', 'none', '--ctx-size', '4096',
                                               '--parallel', '1', '--host', str(self.chat.socket_path), '--no-webui'], environment, True)
            self.record(stage='model_loading', engine_started=True, engine_pid=engine_child.pid)
            while True:
                remaining = self.remaining()
                if self.log_errors:
                    raise RuntimeError('engine_log_capture_failed')
                if engine_child.poll() is not None:
                    raise RuntimeError(f'engine_exited:{engine_child.returncode}')
                status = self.health(self.chat.socket_path, min(0.5, remaining))
                self.remaining()
                with self.lock:
                    offload = re.search(r'offloaded (\d+)/(\d+) layers to GPU', (self.tail['stdout'] + self.tail['stderr']).decode(errors='replace'))
                layers = [int(offload[1]), int(offload[2])] if offload else None
                self.record(engine_http_status=status, offload_layers=layers)
                if status == 200 and layers and layers[0] > 0 and layers[0] == layers[1]:
                    self.record(stage='ready')
                    with self.chat.lock:
                        self.chat.phase = 'ready'
                    break
                self.stop.wait(0.05)
            while not self.stop.wait(0.05):
                if self.log_errors:
                    raise RuntimeError('engine_log_capture_failed')
                if engine_child.poll() is not None:
                    raise RuntimeError(f'engine_exited:{engine_child.returncode}')
            self.record(stage='stopped')
            with self.chat.lock:
                self.chat.phase = 'stopped'
        except Exception as error:
            cancelled = self.stop.is_set()
            with self.chat.lock:
                self.chat.phase = 'cancelled' if cancelled else 'failed'
                self.chat.error = None if cancelled else str(error)[:500]
            try:
                self.record(stage=self.chat.phase, error=self.chat.error)
            except OSError as save_error:
                print(f'startup_diagnostic_save_failed:{type(save_error).__name__}', file=sys.stderr, flush=True)
        finally:
            try:
                code = self.stop_child()
                for thread in threads:
                    thread.join(timeout=1)
                self.record(engine_exit_code=code if engine_child else None, log_errors=self.log_errors,
                            log_capture_complete=all(not thread.is_alive() for thread in threads))
            finally:
                self.finished.set()
