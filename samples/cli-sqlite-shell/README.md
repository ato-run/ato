# SQLite Shell (Terminal)

An interactive SQL shell served as a hosted Terminal: open it in ato.run and
type SQL. No web UI is added; the Terminal is the application surface.

| | |
|---|---|
| Program | Python `sqlite3` command-line shell (`python -m sqlite3`), SQLite 3.40.1 |
| Image | `docker.io/library/python@sha256:b685a4fa…4a34` (3.13.9-slim-bookworm, linux/amd64) |
| Licenses | Python: PSF-2.0. SQLite: public domain. This directory: MPL-2.0 (repository license) |
| Limits | 256 MiB memory, 0.5 CPU, 32 pids, no network, read-only root filesystem, UID 65532 |

The image is pulled by digest from Docker Hub; the Capsule carries only
`launch.py`, this README and `capsule.toml`.

## Contract

`probe_ready_output = "sqlite> "` makes the Terminal observation type only once
the prompt is on screen. It then sends `SELECT 'E' || 'ND';` and the
observation ends at `('END',)` followed by the next prompt. K is the digest of
the whole exchange (banner, prompt, echoed input, answer, prompt):
`sha256:bcaa6a7c52f3415b69fc8941d730a513c154c914c0fcd6b7348d79d4522afb89`.

`launch.py` waits until the Runner has sized the PTY to 120×40 and then
`exec`s the shell, so a resize never redraws the prompt.

## Saving a database

The root filesystem is read-only. When the Run has an output grant,
`ATO_OUTPUT_DIR` is `/.ato-data/outputs` and files written there are saved as
Instance Assets after the Run stops:

```sql
CREATE TABLE notes(body TEXT);
INSERT INTO notes VALUES ('hello');
VACUUM INTO '/.ato-data/outputs/notes.db';
```

Python's shell supports `.help`, `.version` and `.quit`; other SQLite CLI dot
commands are not available.
