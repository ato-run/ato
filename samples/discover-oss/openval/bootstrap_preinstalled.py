"""Start OpenVal with a sealed, offline Linux dependency archive."""

import os
import platform
import sys
import tarfile
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parent
MAX_FILES = 10000
MAX_BYTES = 512 * 1024 * 1024


def main():
    if sys.version_info[:3] != (3, 12, 8):
        raise SystemExit("Python 3.12.8 is required")
    if sys.platform != "linux" or platform.machine().lower() not in ("x86_64", "amd64"):
        raise SystemExit("this dependency archive requires Linux x86-64")
    runtime = Path(os.environ["ATO_RUNTIME_DIR"]).resolve()
    runtime.mkdir(parents=True, exist_ok=True)
    site = runtime / "site"
    site.mkdir()  # Each Run must receive a fresh directory.
    with tarfile.open(ROOT / "site.tar.xz", "r:xz") as archive:
        members = archive.getmembers()
        if len(members) > MAX_FILES or sum(member.size for member in members) > MAX_BYTES:
            raise SystemExit("dependency archive exceeds extraction limits")
        names = set()
        for member in members:
            name = PurePosixPath(member.name)
            if (name.is_absolute() or ".." in name.parts or member.name in names
                    or not (member.isdir() or member.isfile())):
                raise SystemExit("unsafe dependency archive entry")
            names.add(member.name)
        archive.extractall(site, members=members, filter="data")
    temporary = runtime / "temp"
    temporary.mkdir(exist_ok=True)
    env = {
        **os.environ,
        "HOST": "0.0.0.0",
        "OPENVAL_ROOT": str(ROOT),
        "TMPDIR": str(temporary),
        "PYTHONPATH": str(site / "site"),
        "PYTHONNOUSERSITE": "1",
        "PYTHONDONTWRITEBYTECODE": "1",
    }
    os.execve(sys.executable, [sys.executable, "-B", str(ROOT / "server.py")], env)


if __name__ == "__main__":
    main()
