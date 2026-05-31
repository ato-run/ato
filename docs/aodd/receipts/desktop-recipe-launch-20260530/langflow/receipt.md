# langflow (v1.1.0) — Tier A

## AODD Receipt

### Test Date
2026-05-30

### Launch Method
CLI plan-only + Desktop MCP NavigateToUrl

### Result
**BLOCKED** at preflight validation (state bindings) + at runtime (source build)

### Blocker
state_binding_unix_path: Multi-service capsule with state binding Unix paths.

Runtime: Source build (`[build.steps]`) uses `provision.sh`:
```
pip install ... && npm install ...
```
Shell commands fail on Windows (no `&&` in cmd.exe; `export` not recognized).

Desktop log:
```
08:16:14.019 ERROR ato session start failed handle="github.com/langflow-ai/langflow"
```

### Attestations
- [x] CLI preflight blocked by state bindings
- [x] Desktop launch fails at source build
- [x] Dual blocker shared with linkwarden pattern
