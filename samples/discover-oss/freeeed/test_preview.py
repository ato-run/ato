#!/usr/bin/env python3
"""Exercise the local preview with real FreeEed and Tika processes on macOS."""

import argparse
import io
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import zipfile


def unused_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def stop(process: subprocess.Popen) -> None:
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("engine", type=Path)
    parser.add_argument("tika", type=Path)
    parser.add_argument("test_root", type=Path)
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("this test currently requires macOS sandbox-exec")
    engine, tika, root = (path.resolve(strict=True) for path in (args.engine, args.tika, args.test_root))
    run = Path(tempfile.mkdtemp(prefix="freeeed-preview-", dir=root))
    if any(c in str(run) for c in ('"', "\\", "\n", "\r")):
        raise ValueError("unsupported test-root path")
    jobs = run / "jobs"
    jobs.mkdir()
    classes = run / "classes"
    classes.mkdir()
    source = Path(__file__).with_name("ProcessingPreview.java").resolve(strict=True)
    subprocess.run(["javac", "--release", "17", "-d", str(classes), str(source)], check=True)
    tika_port, web_port = unused_port(), unused_port()
    tika_url = f"http://127.0.0.1:{tika_port}"
    preview_url = f"http://127.0.0.1:{web_port}"
    writable = f'(deny file-write*)(allow file-write* (subpath "{run}") (literal "/dev/null"))'
    env = {**os.environ, "TMPDIR": str(run)}
    tika_log = (run / "tika.log").open("wb")
    preview_log = (run / "preview.log").open("wb")
    tika_process = subprocess.Popen(
        ["sandbox-exec", "-p", f"(version 1)(allow default)(deny network-outbound){writable}",
         "java", f"-Djava.io.tmpdir={run}", f"-Duser.home={run}", "-jar", str(tika),
         "--noFork", "--host", "127.0.0.1", "--port", str(tika_port)],
        cwd=run, env=env, stdout=tika_log, stderr=subprocess.STDOUT,
    )
    preview_process = None
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def request(path: str, *, data: bytes | None = None, headers: dict | None = None):
        req = urllib.request.Request(preview_url + path, data=data, headers=headers or {})
        try:
            with opener.open(req, timeout=30) as response:
                return response.status, response.read()
        except urllib.error.HTTPError as error:
            return error.code, error.read()

    try:
        profile = (f'(version 1)(allow default)(deny network-outbound '
                   f'(require-not (remote ip "localhost:{tika_port}"))){writable}')
        preview_process = subprocess.Popen(
            ["sandbox-exec", "-p", profile, "java", f"-Djava.io.tmpdir={run}",
             f"-Duser.home={run}", "-Djava.net.preferIPv4Stack=true", "-cp", str(classes),
             "ProcessingPreview", str(engine), str(jobs), "127.0.0.1", str(web_port), tika_url],
            cwd=run, env=env, stdout=preview_log, stderr=subprocess.STDOUT,
        )
        deadline = time.monotonic() + 25
        while True:
            if tika_process.poll() is not None or preview_process.poll() is not None:
                raise RuntimeError(f"a test process exited; inspect logs in {run}")
            try:
                if request("/health")[0] == 200:
                    break
            except OSError:
                pass
            if time.monotonic() >= deadline:
                raise TimeoutError(f"test services did not start; inspect logs in {run}")
            time.sleep(0.25)

        content = b"Synthetic test document for FreeEed.\nReference: staging fixture only.\n"
        status, _ = request("/process", data=content, headers={"Content-Type": "text/plain", "X-File-Extension": "txt"})
        assert status == 400
        status, _ = request("/process", data=content, headers={"Content-Type": "application/octet-stream"})
        assert status == 400
        status, data = request("/process", data=content, headers={"Content-Type": "application/octet-stream", "X-File-Extension": "txt"})
        assert status == 200
        with zipfile.ZipFile(io.BytesIO(data)) as result:
            assert set(result.namelist()) == {"metadata1.csv", "native1.zip"}
            with zipfile.ZipFile(io.BytesIO(result.read("native1.zip"))) as native:
                assert native.read("native/UPI_00001_document.txt") == content
                assert b"Synthetic test document for FreeEed" in native.read("text/UPI_00001_document.txt.txt")
        assert not list(jobs.iterdir())
        stop(tika_process)
        assert request("/health")[0] == 503
        assert request("/process", data=content, headers={"Content-Type": "application/octet-stream", "X-File-Extension": "txt"})[0] == 500
        assert not list(jobs.iterdir())
        print(f"Preview input, output, failure and cleanup verified: {run}")
    finally:
        if preview_process is not None:
            stop(preview_process)
        stop(tika_process)
        tika_log.close()
        preview_log.close()


if __name__ == "__main__":
    main()
