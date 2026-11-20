"""A stand-in database: opens its SQLite file on the state mount and keeps
its Port open, so the group has a real second serving step."""

import socket
import sqlite3
import sys

port = int(sys.argv[1])
path = sys.argv[2]
sqlite3.connect(path).execute("CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY)")

listener = socket.socket()
listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
listener.bind(("0.0.0.0", port))
listener.listen()
while True:
    connection, _ = listener.accept()
    connection.close()
