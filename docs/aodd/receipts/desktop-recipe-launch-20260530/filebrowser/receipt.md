# filebrowser (v2.32.0) — Tier B

## AODD Receipt

### Test Date
2026-05-30

### Launch Method
CLI plan-only: `ato run --plan-only samples/recipes/filebrowser --yes`

### Result
**BLOCKED** at preflight validation

### Blocker
state_binding_unix_path: Dual bindings:
- `target '/database' must be an absolute path`
- `target '/srv' must be an absolute path`

### Attestations
- [x] CLI preflight blocked
