"""Generate the plain-text source manifest used by the source page."""
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent
OUTPUT = ROOT / 'src' / 'source-manifest.json'
PATHS = ['src', 'source.html', 'index.html', 'vite.config.ts', 'tsconfig.json',
         'package.json', 'pnpm-lock.yaml', 'pnpm-workspace.yaml', 'README.md',
         'LICENSE', 'AGPL-3.0.txt', 'THIRD-PARTY-NOTICES.txt',
         'BROWSER-ADAPTER-README.txt', 'package_source.py']
files = {}
for relative in PATHS:
    path = ROOT / relative
    entries = sorted(path.rglob('*')) if path.is_dir() else [path]
    for file in entries:
        if file.is_file() and file != OUTPUT:
            files[str(file.relative_to(ROOT))] = file.read_text(encoding='utf-8')
OUTPUT.write_text(json.dumps(files, ensure_ascii=False), encoding='utf-8')
