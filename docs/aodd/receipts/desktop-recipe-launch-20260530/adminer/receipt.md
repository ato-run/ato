# adminer (v4.8.1) — Tier B

## AODD Receipt

### Test Date
2026-05-30

### Launch Method
CLI plan-only: `ato run --plan-only samples/recipes/adminer --yes`

### Result
**PASS** at preflight, **DEGRADED** at Desktop runtime

### CLI Preflight
```
preflight: adminer@4.8.1 — no pending requirements; launch can proceed.
```

### Runtime Blocker
Same as pgweb:
1. **podman DNS**: `ato` uses podman with DNS failure for Docker Hub registry
2. **Desktop Focus no WebView pane**: NavigateToUrl consumed, but no capsule pane created

### Attestations
- [x] CLI preflight PASS (no state bindings, simple OCI container)
- [x] Simple single-service recipe with zero state — ideal Windows test candidate
- [x] Blocked only by runtime infrastructure issues
