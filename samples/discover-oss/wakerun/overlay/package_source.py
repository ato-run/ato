"""Embed all build inputs for an offline browser-generated source archive."""
import base64
import json
from pathlib import Path

root = Path(__file__).resolve().parent
output = root / "src/source-manifest.json"
excluded = {".git", ".tmp", "node_modules", "dist", "target"}
files = {}
for path in sorted(root.rglob("*")):
    relative = path.relative_to(root)
    if not path.is_file() or path == output or excluded.intersection(relative.parts):
        continue
    if path.is_symlink():
        raise SystemExit(f"Source symlinks are not supported: {relative}")
    files[relative.as_posix()] = base64.b64encode(path.read_bytes()).decode("ascii")
output.write_text(json.dumps(files, ensure_ascii=True, separators=(",", ":")), encoding="utf-8")
print(f"Embedded {len(files)} source files")
