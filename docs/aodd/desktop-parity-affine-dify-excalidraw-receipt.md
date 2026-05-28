# AODD Receipt: Desktop Parity — AFFiNE, Dify, Excalidraw

**Usecase:** Desktop parity verification for AFFiNE, Dify, and Excalidraw after split landing stack  
**Actor:** Autonomous agent operating through MCP automation (Focus View Desktop mode)  
**Goal:** Each app session-creates, primary URL responds from macOS host, cleanup leaves zero orphan containers  
**Result:** `complete` — 3/3 session-created, 3/3 host-HTTP-ready

---

## Environment

| Field | Value |
|---|---|
| dev commit SHA | `1d20ae3d` (Merge #274 — dev-head dify fixed receipt) |
| OS | Darwin arm64 (macOS) |
| Architecture | aarch64 |
| Podman backend | `podman-machine-default` (applehv) |
| DOCKER_HOST | `unix:///var/folders/98/k9wrs95s7972nb_qn_k8k2kr0000gn/T/podman/podman-machine-default-api.sock` |
| ATO_HOME (run 1 — AFFiNE) | `/var/folders/98/k9wrs95s7972nb_qn_k8k2kr0000gn/T/tmp.khgypgYNYh` (mktemp, isolated) |
| ATO_HOME (run 2 — Dify + Excalidraw) | `/var/folders/98/k9wrs95s7972nb_qn_k8k2kr0000gn/T/tmp.SHfxDCtIpU` (mktemp, isolated) |
| Binary | `crates/ato-desktop/target/debug/ato-desktop` (built from dev HEAD) |
| CLI binary | `target/debug/ato` (built from dev HEAD) |
| Predecessors | #271 (degraded receipt), #274 (dify fixed receipt) |

---

## App Results

### AFFiNE (`capsule://github.com/toeverything/AFFiNE`)

| Field | Result |
|---|---|
| Desktop input | `capsule://github.com/toeverything/AFFiNE` |
| Resolve kind | `sample_recipe` ✅ (not GitHub fallback) |
| Session ID | `ato-desktop-session-20743` |
| Session started | `01:51:35` (UTC) |
| ForceApprovePending sent | `01:49:42` |
| Elapsed (approve → session-created) | ~1m53s |
| WebView URL | `http://127.0.0.1:3010/` |
| Host HTTP | `HTTP/1.1 302` at `:3010` ✅ |
| Container count | 3 |
| Container architectures | all `aarch64` ✅ |
| Containers | `ato-affine-deb1f260-db`, `ato-affine-deb1f260-redis`, `ato-affine-deb1f260-main` |
| Stop via MCP | `stop_active_session` not supported in Focus mode — see findings |
| Cleanup | Containers stopped manually; 0 orphan containers ✅ |

### Dify (`capsule://github.com/langgenius/dify`)

| Field | Result |
|---|---|
| Desktop input | `capsule://github.com/langgenius/dify` |
| Resolve kind | `sample_recipe` ✅ |
| Session ID | `ato-desktop-session-29078` |
| Session started | `02:01:40` (UTC) |
| ForceApprovePending sent | `01:58:05` |
| Elapsed (approve → session-created) | ~3m35s |
| WebView URL | `http://127.0.0.1:5001/` (API port; main web on :3000) |
| Host HTTP | `HTTP/1.1 200 OK` at `:3000` ✅ (native arm64, not 307 as in #271) |
| Container count | 6 |
| Container architectures | all `aarch64` ✅ (native, no x86_64 emulation) |
| Containers | `db`, `redis`, `weaviate`, `api`, `worker`, `main` |
| Stop via MCP | Containers stopped manually; 0 orphan containers ✅ |

**Improvement vs #271:** Dify previously returned HTTP 307 from macOS host under x86_64 emulation.
After #272 removed `allow_emulation = true`, Dify now starts native aarch64 containers and returns HTTP **200** directly.

### Excalidraw (`capsule://github.com/excalidraw/excalidraw`)

| Field | Result |
|---|---|
| Desktop input | `capsule://github.com/excalidraw/excalidraw` |
| Resolve kind | `sample_recipe` ✅ |
| Session ID | `ato-desktop-session-29453` |
| Session started | `02:03:46` (UTC) |
| ForceApprovePending sent | `02:03:28` |
| Elapsed (approve → session-created) | ~18s |
| WebView URL | `http://127.0.0.1:8080/` |
| Host HTTP | `HTTP/1.1 200 OK` at `:8080` ✅ |
| Container count | 1 |
| Container architecture | `aarch64` ✅ |
| Container | `ato-excalidraw-5a67a1f1-main` (`excalidraw/excalidraw`) |
| Stop | Container stopped; 0 orphan containers ✅ |

---

## Final Summary

| App | Session-Created | Host HTTP | Containers | Cleanup |
|---|---|---|---|---|
| AFFiNE | ✅ | 302 :3010 | 3x aarch64 | ✅ 0 orphan |
| Dify | ✅ | 200 :3000 | 6x aarch64 | ✅ 0 orphan |
| Excalidraw | ✅ | 200 :8080 | 1x aarch64 | ✅ 0 orphan |

**3/3 session-created. 3/3 host-HTTP-ready.**

---

## Findings

### Finding 1: `stop_active_session` MCP not supported in Focus mode

`stop_active_session` returns `"automation command Discriminant(23) is not supported in Focus mode (no WebView pane)"`.  
In Focus View mode, the Desktop has no WebView pane to drive stop from MCP.

Workaround used in this run: stop containers directly via `docker stop` + `docker container prune -f`.

The real user-facing stop path (`Cmd+Shift+W` / `StopActiveSession` menu item) is available in the native UI but is not reachable via the Focus mode MCP dispatcher. This means the stop/restart cycle cannot be fully automated via MCP in Focus mode.

**Classification:** Desktop stop/restart automation gap (Focus mode MCP limitation)  
**Does not block:** session-created or HTTP-ready verification  
**Follow-up:** Tracked separately; not a blocker for Desktop parity

### Finding 2: `CloseAppWindow` closes the entire Focus View window

Sending `host_dispatch_action: CloseAppWindow` closes `WindowId(1v1)` (the Focus View root window), making all subsequent MCP actions fail with `"AppWindow update failed: window not found"`. The Desktop process continues running but is windowless.

Workaround: restart Desktop process with a fresh `ATO_HOME`.

**Classification:** Desktop automation gap (Focus mode window lifecycle)  
**Does not block:** session-created or HTTP-ready verification  
**Follow-up:** Expose a "reopen Focus window" action or restrict `CloseAppWindow` to content sub-windows only

### Finding 3: `ato ps --json` returns `[]` for Desktop sessions

Desktop-managed sessions are not reflected in CLI `ato ps --json`. This is expected: Desktop tracks its own session ledger internally. State verification must rely on Desktop log + `docker ps` / `podman ps`.

**Classification:** CLI/Desktop session ledger divergence (documented, expected)  
**Follow-up:** Tracked separately; no action required for this receipt

### Finding 4: Dify WebView opens the API port (:5001), not the main web (:3000)

`AppCapsuleShell: WebView created for running session url=http://127.0.0.1:5001/`.  
The WebView is set to the API port rather than the user-facing web port. The main web UI is accessible at `:3000` from the macOS host. This may cause a blank/JSON page in the embedded WebView but does not affect container health or host HTTP readiness.

**Classification:** Recipe/Desktop integration note  
**Follow-up:** Verify whether the Dify recipe `[server]` stanza should set the primary URL to `:3000`

---

## Launch Commands Used

```bash
# AFFiNE
host_dispatch_action: NavigateToUrl url=capsule://github.com/toeverything/AFFiNE
# wait ~5s
host_dispatch_action: ForceApprovePending

# Dify
host_dispatch_action: NavigateToUrl url=capsule://github.com/langgenius/dify
# wait ~5s
host_dispatch_action: ForceApprovePending

# Excalidraw
host_dispatch_action: NavigateToUrl url=capsule://github.com/excalidraw/excalidraw
# wait ~5s
host_dispatch_action: ForceApprovePending
```

Note: bare alias (`"affine"`, `"dify"`, `"excalidraw"`) does not work in NavigateToUrl.
Must use full `capsule://github.com/<owner>/<repo>` form.

---

## Known Open Follow-ups

| Issue | Status |
|---|---|
| #273 — `ato app session stop` / network prune on stop | open, tracked separately |
| Desktop `stop_active_session` MCP in Focus mode | new finding, follow-up |
| Desktop `CloseAppWindow` closes root Focus window | new finding, follow-up |
| Dify WebView URL points to API port (:5001) instead of web (:3000) | new finding, follow-up |
| Dify worker RabbitMQ | non-blocking; worker uses internal queue |
