# dify (v1.0.0) — Tier B

## AODD Receipt

### Test Date
2026-05-30

### Launch Method
CLI plan-only: `ato run --plan-only samples/recipes/dify --yes`

### Result
**BLOCKED** at preflight validation

### Blocker
state_binding_unix_path: Most complex recipe in the repo (6+ services), all with state binding Unix paths.

Every sub-service (db, redis, app, worker, etc.) declares `[[state_bindings]]` with UNIX absolute targets. Requires comprehensive state binding rework for Windows.

### Attestations
- [x] CLI preflight blocked
- [x] Most complex recipe — represents the hardest case for Windows port
