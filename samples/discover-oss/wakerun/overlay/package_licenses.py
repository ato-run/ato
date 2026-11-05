"""Collect production dependency notices from the locked npm installation."""
import json
import html
import subprocess
from pathlib import Path

root = Path(__file__).resolve().parent
result = subprocess.run(
    ["npm", "ls", "--omit=dev", "--all", "--parseable"], cwd=root,
    text=True, capture_output=True, check=True,
)
entries = []
for line in result.stdout.splitlines()[1:]:
    package = Path(line)
    metadata = json.loads((package / "package.json").read_text(encoding="utf-8"))
    name = metadata["name"]
    notices = sorted(path for path in package.iterdir() if path.is_file() and
                     path.name.lower().startswith(("license", "licence", "copying", "notice", "feel-free")))
    if not notices and name.startswith("@radix-ui/"):
        notices = [root / "browser-license-fallbacks/radix-LICENSE.txt"]
    if not notices and name == "react-remove-scroll-bar":
        notices = [root / "browser-license-fallbacks/react-remove-scroll-bar-LICENSE.txt"]
    if not notices:
        raise SystemExit(f"Missing dependency notice: {name}")
    if name.startswith("@tauri-apps/") and all(path.suffix == ".spdx" for path in notices):
        notices.append(root / "browser-license-fallbacks/tauri-MIT.txt")
    texts = [path.read_text(encoding="utf-8") for path in notices]
    entries.append((name, metadata["version"], str(metadata.get("license", "unknown")), texts))

output = root / "public/THIRD-PARTY-NOTICES.txt"
output.parent.mkdir(exist_ok=True)
with output.open("w", encoding="utf-8") as file:
    file.write("Wakerun upstream: https://github.com/mikaneco/wakerun, revision "
               "e372df3972373fca1209c9b68a476ad06df265e5. Its README declares MIT.\n")
    file.write("The browser adaptation includes the following npm dependencies.\n")
    file.write("Radix and react-remove-scroll-bar package tarballs without license files use "
               "the upstream license copies in browser-license-fallbacks.\n")
    for name, version, license_name, texts in sorted(entries):
        file.write(f"\n{'=' * 40}\n{name} {version} — {license_name}\n{'=' * 40}\n")
        for text in texts:
            file.write(text.rstrip() + "\n")
print(f"Collected notices for {len(entries)} production packages")
page = root / "public/third-party-notices.html"
page.write_text('<!doctype html><html lang="en"><head><meta charset="utf-8">'
                '<meta name="viewport" content="width=device-width, initial-scale=1">'
                '<title>Third-party notices</title></head><body><p><a href="/source.html">'
                'Source</a></p><h1>Third-party notices</h1><pre style="white-space:pre-wrap">' +
                html.escape(output.read_text(encoding="utf-8")) + '</pre></body></html>', encoding="utf-8")
