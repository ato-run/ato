#!/bin/sh
set -eu
python3 - <<'PY'
import os
import socket

sock_path = os.environ["CAPSULE_IPC_ECHO_SOCKET"]
message = b"Hello from Sandbox"
client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
client.connect(sock_path)
client.sendall(message)
client.shutdown(socket.SHUT_WR)
reply = client.recv(1024)
client.close()
print(reply.decode("utf-8"))
PY
