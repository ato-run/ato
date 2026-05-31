# homepage (v0.10.9) — Tier A

## AODD Receipt

### Test Date
2026-05-30

### Launch Method
CLI plan-only: `ato run --plan-only samples/recipes/homepage --yes`

### Result
**BLOCKED** at preflight validation

### Blocker
state_binding_unix_path: Dual bindings:
- `target '/app/config' must be an absolute path`
- `target '/app/public/icons' must be an absolute path`

### Attestations
- [x] CLI preflight blocked
