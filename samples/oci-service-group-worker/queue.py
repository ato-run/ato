"""A stand-in queue broker: accepts and closes connections on its Port."""

import socket
import sys

listener = socket.socket()
listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
listener.bind(("0.0.0.0", int(sys.argv[1])))
listener.listen()
while True:
    connection, _ = listener.accept()
    connection.close()
