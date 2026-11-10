# Workspace W3 — Memos OSS continuity evidence (2026-11-10)

Status: one existing OSS app proven to run and persist locally. Formation/Workspace wiring is not done yet.

## App

| Item | Value |
|---|---|
| Name | Memos (usememos/memos) |
| Purpose | Knowledge / notes (Work group) |
| Version | 26.11.0, `memos_26.11.0_darwin_arm64.tar.gz` |
| Runtime | single Go binary, no external service; SQLite in its own data directory |
| Start | `./memos --port 5230 --data ./data --addr 127.0.0.1` |
| Stop | SIGTERM to the process |
| Storage | `data/memos_prod.db` (+ WAL); the OSS owns this database, not a DataResource |
| Auth | first-run administrator `ato-admin`; instance is private (single-user) |
| Secrets | none recorded here; the password exists only in the local working tree |
| License | AGPL-3.0 (verify before any distribution) |

## Evidence

1. Started the binary; `GET /healthz` -> 200.
2. First-run UI created the administrator.
3. Created the memo `Ato dogfooding memo 2026-11-10` in the real editor and saved it (UI shows it).
4. Stopped the process, started it again with the same `--data` directory.
5. Reloaded the UI: **the memo is still present** after the restart.

```
memo visible (before restart): true
memo after restart:         true
```

Screenshots (working tree, not committed): `.tmp/memos/with-memo.png`, `.tmp/memos/after-restart.png`.

## What is proven / not proven

Proven: a real, unmodified OSS app starts, accepts user data through its own UI, owns its SQLite
database, and keeps the data across a process restart.

Not proven / not done:

- No Formation or Ato Run wraps Memos yet; the Workspace opens it as an external URL only.

## Workspace entry (ato-pwa #794)

The Knowledge card is driven by `VITE_WORKSPACE_KNOWLEDGE_URL`. With the local Memos
instance configured:

```
knowledge card present: true
not set up count: 3        (Back Office cards still say so)
knowledge has open: 1
popup url: http://127.0.0.1:5230/auth?redirect=%2F
page errors: none
```

Without the variable the card says "Not set up yet", so a deployment cannot claim a
Knowledge app it does not have. Screenshot: `.tmp/workspace-knowledge.png`.
- No Ato authentication or single sign-on; the instance has its own account.
- No backup/export rehearsal, and no data migration between Mems and a DataResource (not planned:
  the OSS database stays authoritative).
- License check for redistribution is pending.

## Next

- Add a Formation/run recipe for Memos and open it from the Workspace "Knowledge" card.
- Mark the card "available (external/selfhost)" only once the launch path exists.
- Record backup and upgrade steps before calling the app operational.
