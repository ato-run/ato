#!/usr/bin/env python3
"""Assemble a pinned, browser-only OpenVal preview into a new directory."""

import base64
import concurrent.futures
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import sys
import tarfile
import urllib.request
from zipfile import ZIP_DEFLATED, ZipFile


HERE = Path(__file__).resolve().parent
COMMIT = "89f471751e498813c8458cdc1f6bdc5a54b78e2a"
SOURCE_URL = f"https://codeload.github.com/aanfinson00/openval/tar.gz/{COMMIT}"
SOURCE_SHA = "cb66b3179df2ef6f5e4c85bfd4c5c8a22a22c017dc2f0532d0ada29c20e6e549"
PYODIDE_URL = "https://registry.npmjs.org/pyodide/-/pyodide-0.29.4.tgz"
PYODIDE_SHA512 = "tCseTsqU3kSxZIjkue5zXxTMNEwrKZwOIIEQRBA/VzHxFN1hoCxe4w41phfCdHd9it9RcCNQb5K/Re0InqMgvA=="
LICENSES = {
    "openpyxl.tar.gz": ("https://files.pythonhosted.org/packages/3d/f9/88d94a75de065ea32619465d2f77b29a0469500e99012523b91cc4141cd1/openpyxl-3.1.5.tar.gz", "cf0e3cf56142039133628b5acffe8ef0c12bc902d2aadd3e0fe5878dc08d1050"),
    "et_xmlfile.tar.gz": ("https://files.pythonhosted.org/packages/d3/38/af70d7ab1ae9d4da450eeec1fa3918940a5fafb9055e934af8d6eb0c2313/et_xmlfile-2.0.0.tar.gz", "dab3f4764309081ce75662649be815c4c9081e88f0837825f90fd28317d4da54"),
    "pyodide-LICENSE": ("https://raw.githubusercontent.com/pyodide/pyodide/0.29.4/LICENSE", "1f256ecad192880510e84ad60474eab7589218784b9a50bc7ceee34c2b91f1d5"),
    "cpython-LICENSE": ("https://raw.githubusercontent.com/python/cpython/v3.13.2/LICENSE", "78b12c3a81360b357002334f0e70ea0e92eebf7a9b358805c03c48484945f3bb"),
}


def download(url, digest, algorithm="sha256"):
    with urllib.request.urlopen(url, timeout=120) as response:
        data = response.read()
    actual = getattr(hashlib, algorithm)(data).digest()
    expected = base64.b64decode(digest) if algorithm == "sha512" else bytes.fromhex(digest)
    if actual != expected:
        raise ValueError(f"download hash mismatch: {url}")
    return data


def unpack_files(data, target):
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tar:
        for member in tar:
            if not member.isfile():
                continue
            parts = PurePosixPath(member.name).parts
            if len(parts) < 2 or ".." in parts:
                raise ValueError("invalid archive member")
            path = target.joinpath(*parts[1:])
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(tar.extractfile(member).read())


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: assemble.py NEW_EMPTY_DIRECTORY")
    root = Path(sys.argv[1]).resolve()
    root.mkdir(parents=True, exist_ok=True)
    if any(root.iterdir()):
        raise ValueError("output directory must be empty")
    source = download(SOURCE_URL, SOURCE_SHA)
    (root / "upstream.tar.gz").write_bytes(source)
    app = root / "app"
    unpack_files(source, app)
    runtime = root / "runtime"
    unpack_files(download(PYODIDE_URL, PYODIDE_SHA512, "sha512"), runtime)
    wheels = root / "wheels"
    licenses = root / "licenses"
    wheels.mkdir(); licenses.mkdir()
    jobs = [(entry["source"], entry["sha256"], wheels / entry["filename"]) for entry in json.loads((HERE / "wheel-lock.json").read_text())]
    jobs.extend((url, sha, licenses / name) for name, (url, sha) in LICENSES.items())
    def fetch(job):
        url, sha, path = job
        path.write_bytes(download(url, sha))
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(fetch, jobs))
    subprocess.run([sys.executable, str(HERE / "apply.py"), str(app)], check=True)
    web = app / "web"
    for name in ("package.json", "package-lock.json"):
        shutil.copyfile(HERE / name, web / name)
    config = web / "next.config.mjs"
    text = config.read_text()
    needle = "  reactStrictMode: true,"
    if text.count(needle) != 1:
        raise ValueError("unexpected Next configuration")
    config.write_text(text.replace(needle, needle + '\n  output: "export",'))
    public = web / "public"
    pyodide = public / "pyodide"
    pyodide.mkdir(parents=True)
    for name in ("pyodide.js", "pyodide.asm.js", "pyodide.asm.wasm", "python_stdlib.zip", "pyodide-lock.json"):
        shutil.copyfile(runtime / name, pyodide / name)
    for wheel in wheels.iterdir():
        shutil.copyfile(wheel, pyodide / wheel.name)
    with ZipFile(public / "openval-engine.zip", "w", compression=ZIP_DEFLATED) as archive:
        for file in sorted((app / "src/openval").rglob("*.py")):
            archive.write(file, file.relative_to(app / "src").as_posix())
        archive.write(app / "web/api/_lib.py", "_lib.py")
        archive.write(HERE / "engine.py", "engine.py")
    env = os.environ.copy()
    env["TMPDIR"] = str(root)
    subprocess.run(["npm", "ci", "--ignore-scripts"], cwd=web, env=env, check=True)
    subprocess.run([sys.executable, str(HERE / "source_archive.py"), str(root / "upstream.tar.gz"), str(app), str(wheels), str(licenses), str(public / "source.zip")], check=True)
    # Turbopack keys initial chunks by their exact URL. The Instance gateway
    # namespaces asset URLs, so use webpack's chunk-ID based loader instead.
    subprocess.run(["npm", "run", "build", "--", "--webpack"], cwd=web, env=env, check=True)
    subprocess.run([sys.executable, str(HERE / "externalize_scripts.py"), str(web / "out")], check=True)
    print(f"Static site: {web / 'out'}")


if __name__ == "__main__":
    main()
