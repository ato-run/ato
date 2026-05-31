# linkwarden (v2.9.3) — Tier A

## AODD Receipt

### Test Date
2026-05-30

### Launch Method
CLI plan-only + Desktop MCP NavigateToUrl

### Result
**BLOCKED** at preflight validation (state bindings) + at runtime (source build)

### Blocker
state_binding_unix_path: Triple-service capsule (postgres + meilisearch + app) all use Unix paths.

Additional runtime failure: Source build mode uses `provision.sh` with Unix-isms (`export`, `&&`, etc.) that fail in Windows cmd.exe.

Desktop log:
```
08:12:34.654 ERROR ato session start failed handle="github.com/linkwarden/linkwarden"
  → yarn install fails on Windows
```

### Attestations
- [x] CLI preflight blocked by state bindings
- [x] Desktop launch fails at source build step
- [x] Dual blocker: state + source build
