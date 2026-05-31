# blinko (v0.24.0) — Tier A

## AODD Receipt

### Test Date
2026-05-30

### Launch Method
CLI plan-only + Desktop MCP NavigateToUrl

### Result
**BLOCKED** at preflight validation

### Blocker
state_binding_unix_path: Multi-service (postgres + app)
- postgres: `target '/var/lib/postgresql/data' must be an absolute path`
- app: state bindings use Unix paths

Desktop log confirms:
```
08:12:07.146 ERROR ato session start failed handle="github.com/blinkospace/blinko"
  → orchestration services failed to start in-process
```

### Attestations
- [x] CLI preflight blocked
- [x] Desktop launch attempted, failed at orchestration
