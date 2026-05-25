# AODD Receipt: Desktop Dify WebView opens web port after publish-leaf fix

**Usecase:** Verify that Dify's Desktop primary URL points to the web service (:3000), not the API service (:5001), after #276 merged.

**Result:** `complete`

---

## Context

PR #275 (Desktop parity receipt) documented that Dify's Desktop WebView opened `http://127.0.0.1:5001/` (API port) instead of `http://127.0.0.1:3000/` (web port).

Root cause: `pick_orchestration_leaf_service` in `session.rs` used alphabetical-last fallback when multiple OCI leaves existed. For Dify, `"worker" > "main"` alphabetically, so the worker service (port 5001) was selected.

PR #276 fixed this by adding `network.publish = true` as a tiebreaker (before alphabetical fallback). `resolve_services()` auto-sets `publish=true` for any service named `"main"` with a port, making it the semantic signal for "public-facing service".

---

## Environment

| Field | Value |
|---|---|
| dev SHA | `6c232732a4f632dcdb2c1182e729fcb9ebf8b493` |
| OS / arch | Darwin arm64 (Apple Silicon) |
| Container backend | Podman applehv |
| DOCKER_HOST | `unix:///var/folders/98/k9wrs95s7972nb_qn_k8k2kr0000gn/T/podman/podman-machine-default-api.sock` |
| ATO_HOME | Fresh `mktemp -d` per session |

---

## Dify verification (primary test)

**Command:**
```bash
ATO_HOME="$(mktemp -d)" cargo run -p ato-cli -- app session start dify --json 2>/dev/null
```

**Result:**

| Field | Value |
|---|---|
| status | `ready` |
| source | `sample_recipe` |
| runtime.port | `3000` |
| web.local_url | `http://127.0.0.1:3000/` ✅ (was `:5001` before #276) |
| WebView note | `WebView bound to leaf service 'main' (target='web', port=3000)` |
| host HTTP :3000 | `HTTP/1.1 200 OK` |

**Before #276:** `web.local_url = http://127.0.0.1:5001/` (API port, wrong)  
**After #276:** `web.local_url = http://127.0.0.1:3000/` (web port, correct) ✅

**Container architectures (all native arm64):**

| Container | Arch |
|---|---|
| ato-dify-*-db | aarch64 |
| ato-dify-*-redis | aarch64 |
| ato-dify-*-weaviate | aarch64 |
| ato-dify-*-api | aarch64 |
| ato-dify-*-worker | aarch64 |
| ato-dify-*-main | aarch64 |

**Cleanup:** 0 orphan containers ✅

---

## Regression check: AFFiNE and Excalidraw

The `network.publish` tiebreaker must not disturb single-leaf or existing frontend selection.

### AFFiNE

| Field | Value |
|---|---|
| web.local_url | `http://127.0.0.1:3010/` ✅ |
| WebView note | `WebView bound to leaf service 'main' (target='app', port=3010)` |
| host HTTP :3010 | `HTTP/1.1 302 Found` ✅ |
| cleanup | 0 orphan containers ✅ |

### Excalidraw

| Field | Value |
|---|---|
| web.local_url | `http://127.0.0.1:8080/` ✅ |
| WebView note | `WebView bound to leaf service 'main' (target='app', port=8080)` |
| host HTTP :8080 | `HTTP/1.1 200 OK` ✅ |
| cleanup | 0 orphan containers ✅ |

---

## Summary

| App | primary_url | host HTTP | cleanup |
|---|---|---|---|
| Dify | `:3000` (web) ✅ | 200 ✅ | 0 orphan ✅ |
| AFFiNE | `:3010` ✅ | 302 ✅ | 0 orphan ✅ |
| Excalidraw | `:8080` ✅ | 200 ✅ | 0 orphan ✅ |

All 3 apps select correct primary URL. No regression in `pick_orchestration_leaf_service`.

---

## Known follow-ups (unchanged)

- **#273**: `ato app session stop` should prune session networks
- **#277**: Desktop Focus-mode `stop_active_session` MCP gap
- Dify worker RabbitMQ warning remains non-blocking (does not affect main HTTP readiness)
