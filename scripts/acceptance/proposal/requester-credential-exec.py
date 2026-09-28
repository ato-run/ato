#!/usr/bin/env python3
"""Requester-only credential injector: sealed anonymous memory fd, never a file.

The controller passes only a socket pathname. A separate broker sends the fd
directly to this child; neither controller nor Runtime/Coordinator receive it. Only this short-lived child injects the key into Rust's
environment; the pinned adapter still reserves before std::env::var / send.
"""
import array
import fcntl
import socket
import os
import sys


def main():
    if len(sys.argv) != 4:
        raise ValueError('arguments')
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as channel:
        channel.connect(sys.argv[1])
        marker, ancillary, flags, _ = channel.recvmsg(1, socket.CMSG_SPACE(array.array('i').itemsize))
    if marker != b'K' or flags or len(ancillary) != 1:
        raise ValueError('credential channel')
    level, kind, data = ancillary[0]
    received = array.array('i')
    received.frombytes(data)
    if level != socket.SOL_SOCKET or kind != socket.SCM_RIGHTS or len(received) != 1:
        raise ValueError('credential descriptor')
    fd = received[0]
    seals = fcntl.F_SEAL_WRITE | fcntl.F_SEAL_GROW | fcntl.F_SEAL_SHRINK | fcntl.F_SEAL_SEAL
    if fcntl.fcntl(fd, fcntl.F_GET_SEALS) & seals != seals:
        raise ValueError('unsealed credential memory')
    raw = os.pread(fd, 8193, 0)
    os.close(fd)
    if not 1 <= len(raw) <= 8192 or any(b < 33 or b > 126 for b in raw):
        raise ValueError('credential format')
    # Explicit non-secret environment only. No unrelated inherited credential.
    environment = {name:os.environ[name] for name in ('PATH','HOME','TMPDIR')}
    environment['DEEPSEEK_API_KEY'] = raw.decode('ascii')
    os.execve(sys.argv[2], sys.argv[2:], environment)


if __name__ == '__main__':
    try:
        main()
    except Exception:
        raise SystemExit('requester credential injection failed') from None
