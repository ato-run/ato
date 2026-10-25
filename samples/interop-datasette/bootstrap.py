"""Offline Python realization for the Datasette portability fixture.

The application is upstream Datasette. This small launch configuration only
constructs a clean venv from the bundle's hash-locked wheelhouse and then
replaces itself with Datasette.
"""

from __future__ import annotations

import json
import os
import platform
import shutil
import subprocess
import sys
import venv
from pathlib import Path


ROOT = Path(__file__).resolve().parent


def wheelhouse() -> Path:
    key = (sys.platform, platform.machine().lower())
    supported = {
        ("darwin", "arm64"): "macos-arm64",
        ("linux", "x86_64"): "linux-amd64",
        ("linux", "amd64"): "linux-amd64",
    }
    selected = supported.get(key)
    if selected is None:
        raise SystemExit(f"unsupported Python wheel platform: {key[0]}/{key[1]}")
    return ROOT / "wheels" / selected


def main() -> None:
    if sys.version_info[:2] != (3, 12):
        raise SystemExit(
            f"Python 3.12 is required, selected {platform.python_version()} at {sys.executable}"
        )
    runtime_root = Path(os.environ.get("ATO_RUNTIME_DIR", "/tmp/ato-python-runtime"))
    environment = runtime_root / "venv"
    data_root = runtime_root / "data"
    data_root.mkdir(parents=True, exist_ok=True)
    runtime_database = data_root / "catalog.db"
    shutil.copyfile(ROOT / "catalog.db", runtime_database)
    venv.EnvBuilder(with_pip=True, clear=True, symlinks=True).create(environment)
    python = environment / ("Scripts/python.exe" if os.name == "nt" else "bin/python")
    install_environment = {
        **os.environ,
        "PIP_DISABLE_PIP_VERSION_CHECK": "1",
        "PIP_NO_INDEX": "1",
        "PYTHONDONTWRITEBYTECODE": "1",
    }
    subprocess.run(
        [
            str(python),
            "-m",
            "pip",
            "install",
            "--no-index",
            "--require-hashes",
            "--only-binary=:all:",
            "--find-links",
            str(wheelhouse()),
            "--requirement",
            str(ROOT / "requirements.lock"),
        ],
        check=True,
        env=install_environment,
    )
    port = os.environ.get("ATO_ENDPOINT_APP_HTTP_PORT", "8000")
    print(
        json.dumps(
            {
                "event": "ato_python_runtime_selected",
                "executable": sys.executable,
                "version": platform.python_version(),
                "wheelhouse": wheelhouse().name,
            },
            sort_keys=True,
        ),
        flush=True,
    )
    argv = [
        str(python),
        "-m",
        "datasette",
        "--immutable",
        str(runtime_database),
        "--host",
        "0.0.0.0",
        "--port",
        port,
    ]
    os.execve(str(python), argv, install_environment)


if __name__ == "__main__":
    main()
