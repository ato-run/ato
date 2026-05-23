# Dify OCI Recipe Spike

## Overview

This document records the spike AODD for [langgenius/dify](https://github.com/langgenius/dify) v1.14.2 — an open-source LLM app development platform with RAG, AI workflows, and multi-model support.

**Final status: partial-pass**

The Dify web UI reaches HTTP 200 after startup. DB migrations complete, API process runs, and `ato stop --all` cleanly removes all containers and network. Full interactive functionality is limited by the architectural constraints documented below.

---

## Upstream Compose Summary

Dify's upstream `docker-compose.yaml` includes 40+ services:

- **Core**: `api`, `worker`, `web`, `db` (Postgres), `redis`
- **Vector store**: `weaviate` (default), or `qdrant`, `milvus`, etc.
- **Routing**: `nginx` (reverse proxy, routes `/` → web, `/api` → api)
- **Security**: `ssrf_proxy`, `sandbox`
- **Plugins**: `plugin_daemon`
- **One-shot init**: `init_permissions` (chowns storage volume for uid 1001)

All Dify images (`langgenius/dify-api`, `langgenius/dify-web`) are `linux/amd64` only.

---

## Selected Minimal Service Graph

For the spike, a 6-service graph was chosen:

```
db (postgres:15-alpine)
redis (redis:6-alpine)
weaviate (semitechnologies/weaviate:1.27.0)
  ↓
api (langgenius/dify-api:1.14.2, amd64, emulated)
worker (langgenius/dify-api:1.14.2, amd64, emulated)
  ↓
main/web (langgenius/dify-web:1.14.2, amd64, emulated)
```

**Omitted services:**

| Service | Reason |
|---|---|
| `init_permissions` | No `run_once`/one-shot service support in Ato v0.3 |
| `sandbox` | Requires `cap_add: SYS_ADMIN` / privileged-adjacent config |
| `ssrf_proxy` | Requires bind-mounting a repo config file |
| `plugin_daemon` | Optional for minimal local use |
| `nginx` | Requires bind-mounting upstream repo nginx config |

---

## Env / Secrets

| Variable | Value | Notes |
|---|---|---|
| `SECRET_KEY` | `sk-demo-changeme-...` | **Demo placeholder; must be replaced for production** |
| `DB_PASSWORD` | `difyai123456` | Demo value from upstream compose |
| `WEAVIATE_API_KEY` | `WVF5YThaHlkYwhGUSmCRgsX3tD5ngdN8pkih` | Upstream default demo key |
| `REDIS_PASSWORD` | `difyai123456` | Redis started with `requirepass` via cmd override |
| `CONSOLE_API_URL` | `""` (empty) | Relative URL fallback; see limitation below |

---

## Persistent State

| State key | Container path | Purpose |
|---|---|---|
| `db-data` | `/var/lib/postgresql/data` | Postgres data |
| `weaviate-data` | `/var/lib/weaviate` | Weaviate vector index |
| `api-storage` | `/app/api/storage` | Dify file uploads |

All states use `attach = "explicit"` and require `--state name=/path` at runtime:

```bash
ato run samples/recipes/dify \
  --state db-data=$HOME/my-dify/db-data \
  --state weaviate-data=$HOME/my-dify/weaviate-data \
  --state api-storage=$HOME/my-dify/api-storage
```

---

## Readiness Probes

| Service | Probe | Notes |
|---|---|---|
| `db` | `exec ["pg_isready", "-U", "postgres"]` | Exec, no port required |
| `redis` | `exec ["redis-cli", "-a", "difyai123456", "ping"]` | Exec with password; requirepass configured via cmd override |
| `weaviate` | `http_get /v1/.well-known/ready` | HTTP |
| `api` | `http_get /health`, timeout 300s | HTTP; slow first start due to migrations + emulation |
| `main` | `http_get /`, timeout 180s | HTTP; Next.js SSR page |

---

## AODD Result

**Run date:** 2026-05-23
**Host:** macOS arm64 (Apple Silicon)
**Platform:** `linux/amd64` via `allow_emulation = true`

### Startup sequence

1. All 6 image digests resolved ✅
2. `db`, `redis`, `weaviate` started in parallel (layer 0) ✅
3. `api`, `worker` started after deps ready (layer 1) ✅
4. DB migrations completed — 50+ alembic steps ✅
5. `main` (web) started after api ready (layer 2) ✅
6. `ato ps` shows session at `http://127.0.0.1:36813/` ✅

### Endpoint result

```
GET http://127.0.0.1:36813/
→ HTTP 307 Temporary Redirect → /apps
→ HTTP 200 OK (Dify web UI, Next.js HTML)
```

### Cleanup result

```
ato stop --all
→ 6/6 containers stopped
→ network ato-dify-0337ec41 removed
→ persistent volumes preserved
```

### Total startup time (cold pull)

~15 minutes on Apple Silicon arm64:
- Image pulls: ~10 min (`langgenius/dify-api` ≈ 3.5 GB)
- DB migrations: ~2 min (50+ steps under amd64 emulation)
- App init: ~2 min

Warm restart (images cached): ~3 min.

---

## Final Classification

**partial-pass**

| Criterion | Result |
|---|---|
| Image digest resolution | ✅ pass |
| Dependency start order | ✅ pass |
| Readiness probes fire correctly | ✅ pass |
| DB migrations complete | ✅ pass |
| HTTP 200 on web endpoint | ✅ pass |
| `ato ps` shows session | ✅ pass |
| `ato stop --all` cleans up | ✅ pass |
| No secret leakage | ✅ pass |
| Full interactive UI (API calls work) | ⚠️ partial |
| File upload flows | ⚠️ unverified (init_permissions omitted) |
| Worker file ops | ⚠️ partial (shared state not supported) |

---

## Blockers

### B1: `CONSOLE_API_URL` dynamic port (Ato architecture)

Without nginx, the Dify web's browser-side `fetch` calls to `/console/api/*` resolve relative to the web container's origin. SSR pages render correctly but client-side interactions that call the API directly may fail because the Next.js app expects `CONSOLE_API_URL` to be set to an absolute URL.

**Classification:** Ato architecture — no ingress/proxy layer in v0.3.
**Workaround:** Set `CONSOLE_API_URL=http://host-ip:PORT` with a pinned host port (requires static port mapping, not yet in Ato).
**Follow-up:** Ingress/proxy layer, or static port assignment in recipe.

### B2: `init_permissions` one-shot service (Ato missing feature)

Dify's upstream compose runs `init_permissions` as a one-shot container: `chown -R 1001:1001 /app/api/storage`. Without it, the `api-storage` volume (initially owned by root) may reject writes from the api process (uid 1001). File uploads are not tested in this spike.

**Classification:** Ato missing feature — no `run_once`/one-shot service support.
**Follow-up:** Add `lifecycle = "run_once"` or `one_shot = true` to named targets.

### B3: ~~v0.3 cannot override Docker CMD~~ ✅ Resolved

**Resolved in `feat/runtime-oci-command-override`.**
v0.3 now allows `cmd = [...]` on OCI-typed named targets. Redis is now started with `redis-server --requirepass difyai123456`, and `REDIS_PASSWORD` on api/worker is set to match. The readiness probe now uses `redis-cli -a difyai123456 ping`.

### B4: Shared mutable state (Ato design constraint)

Dify's `api` and `worker` containers share the same named volume for `/app/api/storage` in upstream compose. Ato does not support shared mutable state across services — only `api` (mapped via `services.main`) gets the `api-storage` binding. Worker file operations that write to this path will not be persisted.

**Classification:** Ato design constraint.
**Follow-up:** Consider a `shared_state` or `state_group` concept for read-write volumes shared across services.

---

## Runtime Limitations Discovered

These are new findings from this spike:

| Finding | Impact |
|---|---|
| `tcp_connect` probe requires placeholder name, not port number | Minor — use exec probe for Redis instead |
| ~~v0.3 rejects `cmd` in named OCI targets~~ | ✅ Resolved — `cmd` now allowed for OCI targets |
| No `run_once`/one-shot service | Blocks `init_permissions` pattern (common in multi-service apps) |
| Shared mutable state not supported | Worker/api storage sharing is impossible |
| `allow_emulation = true` required for amd64-only images | Works but adds ~30% overhead on arm64 |

---

## Recipe

See [`samples/recipes/dify/capsule.toml`](../../samples/recipes/dify/capsule.toml).

Usage:
```bash
ato run samples/recipes/dify \
  --state db-data=/path/to/db-data \
  --state weaviate-data=/path/to/weaviate-data \
  --state api-storage=/path/to/api-storage
```

> ⚠️ Replace `SECRET_KEY` with a secure random value before use in any persistent environment.
