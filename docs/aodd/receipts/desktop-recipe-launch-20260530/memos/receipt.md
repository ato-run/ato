# memos (v0.23.1) — Tier A

## AODD Receipt

### Test Date
2026-05-30

### Launch Method
CLI plan-only: `ato run --plan-only samples/recipes/memos --yes`

### Result
**BLOCKED** at preflight validation

### Blocker
state_binding_unix_path: `target '/var/opt/memos' must be an absolute path`

```toml
[[state_bindings]]
target = "/var/opt/memos"
```

Unix-absolute path rejected by manifest validator on Windows.

### Attestations
- [x] CLI preflight blocked
- [x] All Tier A recipes share this same blocker
