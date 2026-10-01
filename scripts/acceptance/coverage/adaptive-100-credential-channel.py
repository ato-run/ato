#!/usr/bin/env python3
"""Separately authorized injector. Run the complete non-secret gate first.

No provider transport, credential file, secret argv or controller environment.
After READY, accept a private stdin payload and serve sealed memfd only to the
same-UID Requester children. One delivery per preregistered callable app.
"""
import argparse
import array
import fcntl
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import threading

def main():
    p = argparse.ArgumentParser()
    p.add_argument('--channel-dir', required=True)
    p.add_argument('command', nargs=argparse.REMAINDER)
    a = p.parse_args(); command = a.command
    if command and command[0] == '--': command = command[1:]
    if sys.platform != 'linux' or not command:
        raise ValueError('Linux/controller required')
    environment = {name: os.environ[name] for name in ('PATH', 'HOME', 'TMPDIR')}
    subprocess.run([*command, '--check-only'], env=environment, stdin=subprocess.DEVNULL,
                   close_fds=True, check=True)
    plan = json.loads(Path(command[command.index('--plan') + 1]).read_text())
    maximum = plan['model_budget']['max_requester_invocations']
    if not 0 < maximum <= 12:
        raise ValueError('delivery budget')
    directory = Path(a.channel_dir); directory.mkdir(mode=0o700)
    address = directory / 's'
    if len(os.fsencode(address)) > 107: raise ValueError('channel path length')
    fd = None; done = threading.Event(); deliveries = []
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as broker:
        try:
            broker.bind(str(address)); broker.listen(1); broker.settimeout(.2)
            print('READY_FOR_REQUESTER_CREDENTIAL', flush=True)
            raw = sys.stdin.buffer.read(32769)
            keys = json.loads(raw)
            if set(keys) != {'DEEPSEEK_API_KEY', 'ATO_DECISION_JEV_API_KEY'}:
                raise ValueError('key scope')
            if not all(isinstance(v, str) and 1 <= len(v) <= 8192 and all(33 <= ord(c) <= 126 for c in v)
                       for v in keys.values()): raise ValueError('format')
            fd = os.memfd_create('adaptive-requester-keys', os.MFD_CLOEXEC | os.MFD_ALLOW_SEALING)
            if os.write(fd, raw) != len(raw): raise ValueError('memory write')
            del keys, raw
            fcntl.fcntl(fd, fcntl.F_ADD_SEALS, fcntl.F_SEAL_WRITE | fcntl.F_SEAL_GROW | fcntl.F_SEAL_SHRINK | fcntl.F_SEAL_SEAL)
            def serve():
                while not done.is_set():
                    try: peer, _ = broker.accept()
                    except TimeoutError: continue
                    with peer:
                        _, uid, _ = struct.unpack('3i', peer.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
                        if uid != os.getuid() or len(deliveries) >= maximum: continue
                        peer.sendmsg([b'K'], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array('i', [fd]))])
                        deliveries.append(1)
            sender = threading.Thread(target=serve, daemon=True); sender.start()
            result = subprocess.run([*command, '--credential-socket', str(address)], env=environment,
                                    stdin=subprocess.DEVNULL, close_fds=True)
            done.set(); sender.join(timeout=1)
            print(json.dumps({'requester_deliveries': len(deliveries), 'maximum': maximum,
                              'credential_values_saved': False}), flush=True)
            return result.returncode
        finally:
            done.set()
            if fd is not None: os.close(fd)
            address.unlink(missing_ok=True); directory.rmdir()

if __name__ == '__main__':
    try: raise SystemExit(main())
    except Exception: raise SystemExit('Credential channel failed; no retry') from None
