# pocketbase (v0.23.0) — Tier A

## AODD Receipt

### Test Date
2026-05-30

### Launch Method
CLI plan-only: `ato run --plan-only samples/recipes/pocketbase --yes`

### Result
**BLOCKED** at preflight validation

### Blocker
state_binding_unix_path: `target '/pb_data' must be an absolute path`

Note: Has `http_ping` health check that would OK on container start, but binding path fails preflight first.

### Attestations
- [x] CLI preflight blocked
