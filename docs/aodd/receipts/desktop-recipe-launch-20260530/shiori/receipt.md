# shiori (v1.7.4) — Tier B

## AODD Receipt

### Test Date
2026-05-30

### Launch Method
CLI plan-only: `ato run --plan-only samples/recipes/shiori --yes`

### Result
**BLOCKED** at preflight validation

### Blocker
state_binding_unix_path: `target '/shiori' must be an absolute path`

Simple single-service OCI container with one state binding. Blocked by same manifest validator issue.

### Attestations
- [x] CLI preflight blocked
