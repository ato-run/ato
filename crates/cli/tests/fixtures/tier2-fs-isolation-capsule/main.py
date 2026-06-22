from pathlib import Path
import sys

safe_path = Path("output") / "safe.txt"
safe_path.parent.mkdir(parents=True, exist_ok=True)
safe_path.write_text("safe-write-ok\n", encoding="utf-8")

blocked_paths = [
    Path("../pwned-outside.txt"),
    Path("../pwn-probe/ato_host_leak_test_17.txt"),
]

for blocked in blocked_paths:
    try:
        blocked.parent.mkdir(parents=True, exist_ok=True)
    except Exception:
        pass

    try:
        blocked.write_text("this must never be written\n", encoding="utf-8")
        print(f"[LEAK] unexpected write success: {blocked}")
        sys.exit(1)
    except (PermissionError, OSError):
        pass

print("tier2 fs isolation enforced")
