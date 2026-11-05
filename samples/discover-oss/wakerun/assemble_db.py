"""Adapt the exact pinned upstream db.ts to the browser SQLite connection.

Run only on a disposable candidate tree; the original checkout is not edited.
The output remains an unverified candidate until its whole UI is tested.
"""
import hashlib
import re
import sys
from pathlib import Path

EXPECTED = "73e186cbbcf458e570a549d3cadc31c4958ec49ee49d1980a800fabb5110a344"
root = Path(sys.argv[1])
path = root / "src/lib/db.ts"
source = path.read_text()
if hashlib.sha256(source.encode()).hexdigest() != EXPECTED:
    raise SystemExit("Unsupported upstream db.ts; inspect changes before adapting")
source = source.replace(
    'import Database from "@tauri-apps/plugin-sql";',
    'import { BrowserDatabase, lockedDatabase, serialized } from "@/lib/browser-sql";',
    1,
)
old = '''let dbInstance: Database | null = null;

export async function getDb(): Promise<Database> {
  if (!dbInstance) {
    dbInstance = await Database.load("sqlite:wakerun.db");
  }
  return dbInstance;
}'''
new = '''const publicDatabase = new BrowserDatabase();

export async function getDb(): Promise<BrowserDatabase> {
  return publicDatabase;
}'''
if source.count(old) != 1:
    raise SystemExit("Upstream database initializer changed")
source = source.replace(old, new).replace("await getDb()", "await lockedDatabase()")
names = re.findall(r"^export async function (\w+)\(", source, flags=re.MULTILINE)
names.remove("getDb")
if len(names) < 40 or len(set(names)) != len(names):
    raise SystemExit("Unexpected upstream public database functions")
for name in names:
    source = source.replace(f"export async function {name}(", f"async function {name}(", 1)
source += "\n// Each public business operation holds one browser-wide SQLite lock.\n"
for name in names:
    source += f"const __wrapped_{name} = serialized({name});\nexport {{ __wrapped_{name} as {name} }};\n"
path.write_text(source)
print(f"Adapted {len(names)} upstream database operations")
