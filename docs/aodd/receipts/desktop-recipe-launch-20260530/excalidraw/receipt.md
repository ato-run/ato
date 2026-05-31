# excalidraw (v0.18.0) — Tier B

## AODD Receipt

### Test Date
2026-05-30

### Launch Method
Desktop MCP: `host_dispatch_action(NavigateToUrl, {url: "capsule://excalidraw/excalidraw"})`

### Expected
After consent window approval, capsule resolves → OCI container starts → WebView pane shows excalidraw UI → `browser_snapshot` returns rendered canvas.

### Actual
1. `ForceApprovePending` consumed pending target (handle: "excalidraw/excalidraw")
2. Preflight SKIPPED: `"preflight collection skipped; unsupported preflight target 'excalidraw/excalidraw': registry handles are not supported by side-effect-free preflight"`
3. Resolve attempted: `ato app resolve excalidraw/excalidraw --json` → FAILED: `"Capsule not found: excalidraw/excalidraw"`
4. No capsule window created; `browser_tabs` returns `{"panes":[]}`

### CLI Preflight
```
ato run --plan-only samples/recipes/excalidraw --yes
→ preflight: excalidraw@0.18.0 — no pending requirements; launch can proceed.
```

### Failure Analysis
Two distinct failures:
1. **Registry handle not found**: The `capsule://excalidraw/excalidraw` URL (without `github.com/` prefix) is treated as registry handle lookup, which fails because the recipe isn't published. Correct URL would be `capsule://github.com/excalidraw/excalidraw`.
2. **Desktop Focus-mode no WebView pane**: Even with the correct URL (tested in earlier session), `open_boot_window` + `start_boot_launch` run but no capsule window appears. Focus dispatcher creates no `CapsuleAppWindow`.

### Related Logs
```
10:16:36.261 INFO  desktop launch input selected handle=excalidraw/excalidraw
10:16:36.802 WARN  preflight collection skipped; falling back to lazy aggregation
10:16:36.802 INFO  resolving capsule handle="excalidraw/excalidraw"
10:16:42.941 ERROR ato helper command failed args=app resolve excalidraw/excalidraw --json
  → Capsule not found: excalidraw/excalidraw
```

### Attestations
- [x] URL used without `github.com/` prefix — treated as registry handle lookup
- [x] Preflight succeeds when run via CLI with local path
- [x] Desktop Focus mode creates no capsule WebView pane
