# affine (v0.20.0) — Tier B

## AODD Receipt

### Test Date
2026-05-30

### Launch Method
CLI plan-only + Desktop MCP NavigateToUrl

### Result
**BLOCKED** at preflight validation  

### Blocker
state_binding_unix_path: Multi-service capsule (postgres + redis + app) each have `[[state_bindings]]` with Unix absolute paths.

- postgres: `/var/lib/postgresql/data`
- app: `/app/data`

### Desktop Log
```
08:11:52.963 INFO  resolving capsule handle="github.com/toeverything/AFFiNE"
```
Redis container is already running from earlier test (`docker ps` shows `ato-affine-...redis`), but the full capsule launch fails on state binding validation.

### Attestations
- [x] CLI preflight blocked by state binding validation
- [x] Desktop launch fails at resolve stage
