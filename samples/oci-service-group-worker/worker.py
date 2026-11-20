"""A background worker: no Port, no listener, no readiness socket.

It is part of the same group as the Web, DB and queue services and is stopped
with them.
"""

import time

while True:
    time.sleep(1)
