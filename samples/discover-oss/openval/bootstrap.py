"""Build an offline, hash-checked environment for the OpenVal candidate."""

import os
import platform
import subprocess
import sys
import venv
from pathlib import Path

ROOT = Path(__file__).resolve().parent


def main():
    if sys.version_info[:2] != (3, 12):
        raise SystemExit("Python 3.12 is required")
    target = {
        ("darwin", "arm64"): "macos-arm64",
        ("linux", "x86_64"): "linux-amd64",
        ("linux", "amd64"): "linux-amd64",
    }.get((sys.platform, platform.machine().lower()))
    if target is None:
        raise SystemExit("no wheelhouse for this platform")
    runtime = Path(os.environ["ATO_RUNTIME_DIR"]).resolve()
    runtime.mkdir(parents=True, exist_ok=True)
    temporary = runtime / "temp"
    temporary.mkdir(exist_ok=True)
    environment = runtime / "venv"
    venv.EnvBuilder(with_pip=True, clear=True, symlinks=True).create(environment)
    python = environment / "bin" / "python"
    env = {
        **os.environ,
        "HOST": "0.0.0.0",
        "OPENVAL_ROOT": str(ROOT),
        "TMPDIR": str(temporary),
        "PIP_NO_INDEX": "1",
        "PIP_NO_CACHE_DIR": "1",
        "PIP_DISABLE_PIP_VERSION_CHECK": "1",
        "PYTHONDONTWRITEBYTECODE": "1",
    }
    subprocess.run(
        [
            str(python), "-m", "pip", "install", "--no-index", "--require-hashes",
            "--only-binary=:all:", "--find-links", str(ROOT / "wheels" / target),
            "--requirement", str(ROOT / "requirements.lock"),
        ],
        check=True,
        env=env,
    )
    os.execve(str(python), [str(python), "-B", str(ROOT / "server.py")], env)


if __name__ == "__main__":
    main()
