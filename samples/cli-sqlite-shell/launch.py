"""Start the SQLite shell once the Runner has sized the PTY.

The Runner resizes the terminal right after the container starts. A prompt
drawn before that would be redrawn by readline on SIGWINCH, so the startup
bytes would depend on timing. Waiting for the declared size first keeps them
identical on every Run; the shell itself is unchanged.
"""

import os
import sys
import time

COLS, ROWS = 120, 40
deadline = time.monotonic() + 10
while time.monotonic() < deadline:
    try:
        if tuple(os.get_terminal_size(0)) == (COLS, ROWS):
            break
    except OSError:
        pass
    time.sleep(0.01)
else:
    sys.exit("terminal was not sized to %dx%d" % (COLS, ROWS))

os.execv(sys.executable, [sys.executable, "-m", "sqlite3", *sys.argv[1:]])
