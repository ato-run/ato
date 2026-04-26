from datetime import datetime
from pathlib import Path

notes = Path("workspace/notes.md")
notes.parent.mkdir(exist_ok=True)
notes.touch()

with notes.open("a") as f:
    f.write(f"- {datetime.now().isoformat()} run\n")

content = notes.read_text()
print(f"workspace/notes.md ({len(content.splitlines())} entries):\n{content}")
