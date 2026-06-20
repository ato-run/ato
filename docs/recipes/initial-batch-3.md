# Recipe Batch 3 — AODD Results

Branch: `feat/initial-recipe-batch-3`  
ATO_HOME: clean per-run (`~/.ato-batch3-<unique>`)  
OS/Arch: macOS darwin/arm64  
Ato version: v0.5.2

## Summary

> **Note:** For consolidated catalog status, see [initial-catalog-status.md](./initial-catalog-status.md).
> For secret cleanup plan, see [secret-cleanup-plan.md](./secret-cleanup-plan.md).

All 5 Batch 3 recipe targets achieved clean AODD pass. Batch 3 covers the
"easy-win catalog" tier: stateless utilities (Stirling-PDF, pgweb), lightweight
single-container apps (shiori), AI showcase (lobe-chat), and mid-complexity
multi-service (linkwarden = 3 services).

## Results Table

| App | Status | Recipe path | Startup (cached) | Endpoint | State | Cleanup | Notes |
|---|---|---|---:|---|---|---|---|
| Stirling-PDF | ✅ pass | `samples/recipes/stirling-pdf` | 35s | HTTP 200 `/login` | none | clean | readiness probe must use `/login` not `/` |
| pgweb | ✅ pass | `samples/recipes/pgweb` | 30s | HTTP 200 `/` | none | clean | standalone, no DB needed |
| shiori | ✅ pass | `samples/recipes/shiori` | 15s | HTTP 200 `/` | `data` | clean | SQLite, very fast |
| lobe-chat | ✅ pass | `samples/recipes/lobe-chat` | 70s | HTTP 307 `/→/chat` | none | clean | lite mode, localStorage |
| linkwarden | ✅ pass | `samples/recipes/linkwarden` | 25s | HTTP 200 `/` | `pgdata`, `data`, `meili` | clean | 3-service; first-run pull ~4min |

## App Summaries

### Stirling-PDF

- **What it is**: Web-based PDF tool suite (split, merge, compress, OCR, etc.)
- **Recipe path**: Single OCI container, port 8080
- **What Ato proved**: Stateless single-container apps launch cleanly with no state config
- **Discovery**: `GET /` always returns HTTP 401 even with `DOCKER_ENABLE_SECURITY=false` (Spring Security default). Readiness probe uses `GET /login` (HTTP 200).
- **Follow-up**: Document default admin credentials; add `DOCKER_ENABLE_SECURITY=false` to recipe with note that auth still activates on first run.

### pgweb

- **What it is**: Lightweight Postgres web admin UI
- **Recipe path**: Single OCI container, port 8081, standalone mode
- **What Ato proved**: Stateless utility apps work with zero config; no DATABASE_URL required for demo mode
- **Notes**: Shows DB connection form when started without DATABASE_URL.

### shiori

- **What it is**: Bookmark manager with web UI and API
- **Recipe path**: Single OCI container, port 8080, SQLite state at `/shiori`
- **What Ato proved**: Persistent SQLite state survives stop/restart via Ato-managed state bind
- **Notes**: Default credentials: `shiori` / `gopher` (displayed on login page).

### lobe-chat

- **What it is**: AI chat UI with plugin ecosystem (LobeHub)
- **Recipe path**: Single OCI container, port 3210, lite/standalone mode
- **What Ato proved**: Next.js apps with HTTP 307 redirects are correctly treated as ready by readiness probe
- **Notes**: Lite mode uses localStorage — no database needed. Full auth mode requires `DATABASE_URL` + `NEXT_AUTH_SECRET`.

### linkwarden

- **What it is**: Bookmark and archive manager with full-text search
- **Recipe path**: Multi-service OCI (postgres + meilisearch + app), 3 states
- **What Ato proved**: Multi-service dependency ordering (db → search → app) works; all 3 containers stop cleanly
- **Discovery**: First-run image pull for 3 large services takes ~4 minutes, causing readiness timeout if images not cached. Second run with cached images: 25s.
- **Follow-up**: Document first-run image pull time; consider pre-pull hint in recipe.

## Runtime Discoveries

### linkwarden first-run pull time

When all 3 service images are cold (not in local Podman cache), total pull time
can exceed the default readiness probe timeout. This is an upstream image size
issue, not an Ato runtime bug. Mitigation options:

1. Increase `timeout_seconds` for first-run heavy apps (per-target control).
2. Add a `prefetch` step or recipe note to pull images before first `ato run`.
3. Track as follow-up: "Add first-run pull-time note to multi-service recipes."

### Stirling-PDF auth-on-by-default

`DOCKER_ENABLE_SECURITY=false` env var does not fully disable Spring Security
profile on this image version. `GET /` returns HTTP 401 by default.
Readiness probe must target `/login`.

## Follow-up Issues

1. **linkwarden first-run pull time**: Multi-service recipes with large images
   should document expected first-run time. Consider per-recipe `timeout_seconds`
   override or pre-pull note.
   
2. **lobe-chat full auth mode**: Lite mode is demo-only. Production recipe variant
   needs `DATABASE_URL` + `NEXT_AUTH_SECRET` + `NEXTAUTH_URL`.

3. **Stirling-PDF default credentials**: Document that admin user is created on
   first start; credentials visible in container logs.

4. **Image tag pinning**: All Batch 3 recipes use pinned version tags. Periodic
   update cycle needed as upstream releases new versions.

5. **linkwarden NEXTAUTH_SECRET**: Recipe uses a demo placeholder. Production
   deployments must override with a strong random secret.

## Batch 4 Candidates

From the P1/P2 lists, next natural batch:

- `actualbudget/actual` — local-first finance (SQLite, single container)
- `gethomepage/homepage` — personal dashboard (single container, YAML config)
- `changedetection-io/changedetection.io` — website change monitor
- `pocketbase/pocketbase` — lightweight backend with built-in UI
- `axllent/mailpit` — already in Batch 2; confirmed pass ✅

AI showcase spike (separate branch):
- `langgenius/dify`
- `FlowiseAI/Flowise`
- `langfuse/langfuse`

## Validation

```
cargo fmt --all                             ✅
cargo check -p capsule -p ato-cli     ✅
cargo test -p ato-cli oci_multi_service    ✅
cargo test -p ato-cli oci_session          ✅
cargo test -p ato-cli oci_compose          ✅
cargo test -p ato-cli oci_provider         ✅
cargo test -p capsule docker_run_script ✅
cargo test -p capsule oci_compose_lock  ✅
git diff --check                           ✅
```
