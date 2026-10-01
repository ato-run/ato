#!/usr/bin/env python3
"""Inject exactly two keys into the short-lived Requester, through sealed RAM."""
import array
import fcntl
import json
import os
import socket
import sys

def main():
    if len(sys.argv) != 4:
        raise ValueError('arguments')
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as channel:
        channel.connect(sys.argv[1])
        marker, ancillary, flags, _ = channel.recvmsg(1, socket.CMSG_SPACE(array.array('i').itemsize))
    if marker != b'K' or flags or len(ancillary) != 1:
        raise ValueError('channel')
    level, kind, data = ancillary[0]
    received = array.array('i'); received.frombytes(data)
    if level != socket.SOL_SOCKET or kind != socket.SCM_RIGHTS or len(received) != 1:
        raise ValueError('descriptor')
    fd = received[0]
    seals = fcntl.F_SEAL_WRITE | fcntl.F_SEAL_GROW | fcntl.F_SEAL_SHRINK | fcntl.F_SEAL_SEAL
    if fcntl.fcntl(fd, fcntl.F_GET_SEALS) & seals != seals:
        raise ValueError('unsealed')
    raw = os.pread(fd, 32769, 0); os.close(fd)
    keys = json.loads(raw); del raw
    if set(keys) != {'DEEPSEEK_API_KEY', 'ATO_DECISION_JEV_API_KEY'}:
        raise ValueError('key scope')
    if not all(isinstance(v, str) and 1 <= len(v) <= 8192 and all(33 <= ord(c) <= 126 for c in v)
               for v in keys.values()):
        raise ValueError('format')
    environment = {name: os.environ[name] for name in ('PATH', 'HOME', 'TMPDIR')}
    environment.update(keys); environment['ATO_DECISION_JEV_MODEL'] = 'jev-1.13.0'
    os.execve(sys.argv[2], sys.argv[2:], environment)

if __name__ == '__main__':
    try: main()
    except Exception: raise SystemExit('Requester injection failed') from None
