# n8n (v1.79.0) — Tier B

## AODD Receipt

### Test Date
2026-05-30

### Launch Method
CLI plan-only: `ato run --plan-only samples/recipes/n8n --yes`

### Result
**BLOCKED** at preflight validation

### Blocker
state_binding_unix_path: `target '/home/node/.n8n' must be an absolute path`

The capsule.toml for n8n declares:
```toml
[[state_bindings]]
target = "/home/node/.n8n"
```

Unix-absolute path is rejected by manifest validator on Windows.

### Attestations
- [x] CLI plan-only confirms validation failure
- [x] Blocker identical to all Tier A recipes
