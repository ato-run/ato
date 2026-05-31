"""Manual Windows regression harness for the desktop session-start pipe hang (#377).

ato-desktop launches `ato app session start --json` via `Command::output()`,
which only returns once the child exits AND its stdout/stderr pipes reach EOF
(every write handle closed). On Windows, children spawned during launch inherit
copies of this process's stdout/stderr pipe write ends (`bInheritHandles=TRUE`).
The OCI `<engine> logs --follow` streamer (and the parent-death watcher) are
long-lived, so before the fix they pinned those pipes open after session start
had already exited — `output()` blocked forever and no WebView pane was created.

`start_session` now clears `HANDLE_FLAG_INHERIT` on stdout/stderr in JSON mode
before any child spawns, so the pipes reach EOF the moment session start exits.

This harness reproduces the parent side: it spawns `ato app session start --json`
with piped stdout/stderr (exactly like the desktop), then watches whether the
pipes reach EOF after the session-start process exits.

    PASS  -> pipes reach EOF shortly after session-start exits
    FAIL  -> session-start exits but the pipes stay open (a child pinned them)

Requirements: Windows, a running container engine (Docker Desktop / Podman), and
a built `ato` binary. This is a *manual* harness (needs Docker), not a unit test.

Overrides (all optional):
    ATO_BIN      path to ato.exe        (default: <repo>/target/debug/ato.exe)
    ATO_HANDLE   capsule handle to launch (default: github.com/sosedoff/pgweb)
    ATO_TIMEOUT  seconds to wait for EOF  (default: 150)

Usage:
    python crates/ato-cli/tests/manual/windows_session_pipe_inheritance.py
"""
import os
import subprocess
import sys
import threading
import time
from pathlib import Path


def repo_root() -> Path:
    # tests/manual/<file> -> crates/ato-cli/tests/manual -> repo root is 4 up.
    here = Path(__file__).resolve()
    return here.parents[4]


def ato_bin() -> Path:
    env = os.environ.get("ATO_BIN")
    if env:
        return Path(env)
    exe = "ato.exe" if os.name == "nt" else "ato"
    return repo_root() / "target" / "debug" / exe


def main() -> int:
    binary = ato_bin()
    handle = os.environ.get("ATO_HANDLE", "github.com/sosedoff/pgweb")
    timeout = float(os.environ.get("ATO_TIMEOUT", "150"))

    if not binary.exists():
        print(f"ato binary not found: {binary}\n"
              f"Build it first: cargo build -p ato-cli --bin ato", file=sys.stderr)
        return 2

    print(f"ato      = {binary}")
    print(f"handle   = {handle}")
    print(f"timeout  = {timeout}s")

    t0 = time.time()
    proc = subprocess.Popen(
        [str(binary), "app", "session", "start", handle, "--json",
         "--run-config-hash", "pipe-inheritance-regression"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=dict(os.environ, ATO_DESKTOP_PARENT_PID=str(os.getpid())),
    )
    print(f"spawned session-start pid={proc.pid}")

    # Drain both pipes to EOF, mirroring std `Command::output()` semantics.
    out_buf: list[bytes] = []
    err_buf: list[bytes] = []

    def drain(stream, buf):
        for chunk in iter(lambda: stream.read(4096), b""):
            buf.append(chunk)

    to = threading.Thread(target=drain, args=(proc.stdout, out_buf))
    te = threading.Thread(target=drain, args=(proc.stderr, err_buf))
    to.start()
    te.start()

    def watch_exit():
        code = proc.wait()
        print(f"[{time.time() - t0:5.1f}s] session-start exited code={code}")

    threading.Thread(target=watch_exit, daemon=True).start()

    deadline = t0 + timeout
    eof = False
    while time.time() < deadline:
        if not to.is_alive() and not te.is_alive():
            eof = True
            break
        time.sleep(0.5)

    elapsed = time.time() - t0
    if eof:
        print(f"[{elapsed:5.1f}s] PASS: both pipes reached EOF (output() would return)")
        return 0
    print(f"[{elapsed:5.1f}s] FAIL: pipes still open after session-start exit "
          f"(a child pinned the pipe — #377 regression)")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
