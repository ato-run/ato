#!/usr/bin/env python3
"""Separately authorized secret injector, not the execution controller.

Run the controller's complete read-only preflight first. Only after its successful
exit is the caller asked to provide a credential on stdin. Keep it in sealed
anonymous memory; pass its fd directly to requester children over a private Unix
socket. Never put it in the controller environment, argv, files or output.
No provider transport. The pinned Rust adapter still reserves before env read.
"""
import argparse
import array
import fcntl
import os
import pathlib
import socket
import struct
import subprocess
import sys
import threading


def serve(broker, fd, done):
    deliveries = 0
    while not done.is_set():
        try:
            peer, _ = broker.accept()
        except TimeoutError:
            continue
        with peer:
            _, uid, _ = struct.unpack('3i', peer.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            if uid != os.getuid() or deliveries >= 7:  # Six cells plus G5 GET-only restart.
                continue
            peer.sendmsg([b'K'], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array('i', [fd]))])
            deliveries += 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('driver', 'ato', 'api', 'plan', 'binaries', 'preflight-run', 'run', 'journal', 'channel-dir'):
        parser.add_argument('--'+name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    if sys.platform != 'linux':
        raise ValueError('Linux injector required')
    common = [item for name in ('ato','api','plan','binaries','journal')
              for item in ('--'+name, str(getattr(args, name)))]
    # All subprocesses start without unrelated host credentials, and no inherited
    # fd. This process is the only component reading the input secret stream.
    environment = {name:os.environ[name] for name in ('PATH','HOME')}
    if 'TMPDIR' in os.environ:
        environment['TMPDIR'] = os.environ['TMPDIR']
    subprocess.run([sys.executable, str(args.driver), 'preflight', *common,
                    '--run', str(args.preflight_run)], env=environment,
                   stdin=subprocess.DEVNULL, close_fds=True, check=True)
    if args.run.exists() or args.channel_dir.exists():
        raise ValueError('fresh run/channel directory required')
    address = args.channel_dir/'s'
    if len(os.fsencode(address)) > 107:
        raise ValueError('short private channel path required')
    args.channel_dir.mkdir(mode=0o700)
    fd = None
    done = threading.Event()
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as broker:
        try:
            broker.bind(str(address)); broker.listen(1); broker.settimeout(.2)
            print('READY_FOR_REQUESTER_CREDENTIAL', flush=True)
            raw = sys.stdin.buffer.read(8193)
            if not 1 <= len(raw) <= 8192 or any(b < 33 or b > 126 for b in raw):
                raise ValueError('credential format')
            fd = os.memfd_create('requester-credential', os.MFD_CLOEXEC | os.MFD_ALLOW_SEALING)
            if os.write(fd, raw) != len(raw):
                raise ValueError('credential memory write')
            del raw
            fcntl.fcntl(fd, fcntl.F_ADD_SEALS, fcntl.F_SEAL_WRITE | fcntl.F_SEAL_GROW | fcntl.F_SEAL_SHRINK | fcntl.F_SEAL_SEAL)
            sender = threading.Thread(target=serve, args=(broker, fd, done), daemon=True)
            sender.start()
            result = subprocess.run([sys.executable, str(args.driver), 'execute', *common,
                                     '--run', str(args.run), '--credential-socket', str(address)],
                                    env=environment, stdin=subprocess.DEVNULL, close_fds=True)
            done.set(); sender.join(timeout=1)
            return result.returncode
        finally:
            done.set()
            if fd is not None:
                os.close(fd)
            address.unlink(missing_ok=True)
            args.channel_dir.rmdir()


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except Exception:
        raise SystemExit('requester credential channel failed; no retry') from None
