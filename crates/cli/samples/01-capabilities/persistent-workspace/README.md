# persistent-workspace

Demonstrates `ato`'s workspace persistence across runs. The script appends a timestamped entry to `workspace/notes.md` on every execution. Running it twice shows two entries, proving state survives across invocations.

## What this proves

- `workspace/` directory is mounted and writable inside the capsule
- State written in one run is readable in the next (`ato decap` promotes to a named workspace)
- The capsule itself remains read-only; only `workspace/` accumulates state

## Run it

```bash
ato run .   # first run → 1 entry
ato run .   # second run → 2 entries
```

Expected output (second run):

```
workspace/notes.md (2 entries):
- 2026-04-21T12:00:00.000000 run
- 2026-04-21T12:00:05.000000 run
```
