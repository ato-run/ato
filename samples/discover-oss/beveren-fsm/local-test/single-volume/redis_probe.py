"""Check one synthetic local Redis value before or after restart."""
import socket
import sys
import time

for _ in range(30):
    try:
        connection = socket.create_connection(('127.0.0.1', 6379), 2)
        break
    except OSError:
        time.sleep(1)
else:
    raise RuntimeError('Redis unavailable')

with connection:
    if sys.argv[1] == 'set':
        connection.sendall(b'*3\r\n$3\r\nSET\r\n$6\r\nmarker\r\n$9\r\nsynthetic\r\n')
        expected = b'+OK\r\n'
    elif sys.argv[1] == 'verify':
        connection.sendall(b'*2\r\n$3\r\nGET\r\n$6\r\nmarker\r\n')
        expected = b'$9\r\nsynthetic\r\n'
    else:
        raise ValueError('Expected set or verify')
    response = b''
    while len(response) < len(expected):
        chunk = connection.recv(len(expected) - len(response))
        if not chunk:
            break
        response += chunk
    assert response == expected
print('Synthetic Redis check passed:', sys.argv[1])
