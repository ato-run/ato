# Initial Catalog Status

Comprehensive status of all Ato recipe catalog targets after Batches 1–3, Exhaustive AODD, retry passes, and Dify/Langfuse spikes.

**Branch:** `feat/catalog-productization`  
**Base:** post-merge of PRs #207, #213, #215, #216, #223, #224, #225, #226 (depends_on, readiness timing, platform emulation, exec probe cleanup)  
**Date:** 2026-05-24  
**Platform:** Darwin arm64 (Apple Silicon), Podman 5.7.1, Ato v0.5.2  
**ATO_HOME policy:** fresh `mktemp -d "$HOME/.ato-catalog.XXXXXX"` per run

---

## Totals

| Classification | Count |
|---|---:|
| **pass** | **34** |
| **partial** | **7** |
| **blocked** | **12** |
| **rejected** | **15** |
| **deferred** | **5** |
| **Total evaluated** | **73** |

### Key runtime capabilities proven

- OCI single-service capsule.toml: start/stop/HTTP readiness/state binds
- OCI multi-service with `depends_on`: topological start/stop, exec probes
- Platform emulation (`allow_emulation = true`): linux/amd64 images on arm64
- Dynamic port allocation: no port conflicts on developer machines
- Readiness probe timing: `initial_delay_seconds`, `timeout_seconds`, `interval_seconds` per target
- Exec probes: `pg_isready`, `redis-cli ping` for internal services
- State persistence: SQLite, Postgres data, vector store, file storage across stop/restart
- Clean teardown: `ato stop --all` removes all containers, network, session records

### Top remaining blockers

1. **Private/restricted upstream images** — twentyhq/twenty-server:0.50.0 returns 403 (private repo). No Ato-side fix.
2. **AMD64-only images without emulation policy** — most arm64-native images exist now, but some AI stacks are amd64-only. Ato supports `allow_emulation = true` but emulation adds ~30% overhead.
3. **No `run_once`/one-shot service** — `init_permissions` pattern (chown at startup) common in multi-service apps. Blocks full Dify integration and similar init-container patterns.
4. **Large image cold pull >5 min** — mindsdb (>5 GB), photoprism (>3 GB), dify-api (~3.5 GB). Per-recipe pull timeout not yet in Ato.
5. **Shared mutable state across services** — Dify worker/api share a volume; Ato does not support shared state bindings.
6. **No ingress/proxy layer** — apps like Dify that expect nginx for routing between web and API need static port assignment or a built-in proxy.

---

## Full Recipe Table

| App | Category | Status | Runtime shape | Services | Recipe path | Startup | Endpoint | Notes |
|---|---|---|---|---|---|---|---:|---|
| memos | AI / LLM apps | pass | OCI single | 1 | `samples/recipes/memos/` | ~15s | HTTP 200 | `stable` tag |
| uptime-kuma | Monitoring / utilities | pass | OCI single | 1 | `samples/recipes/uptime-kuma/` | ~18s | HTTP 302→200 | `:1` major tag |
| n8n | AI / LLM apps | pass | OCI single | 1 | `samples/recipes/n8n/` | ~45s | HTTP 200 | single-user mode |
| nocodb | Data / internal tools | pass | OCI single | 1 | `samples/recipes/nocodb/` | ~35s | HTTP 200 | `:latest` — pin follow-up |
| open-webui | AI / LLM apps | partial | OCI single | 1 | `samples/recipes/open-webui/` | ~220s | HTTP 200 | HF model downloads on first run |
| mailpit | Developer tools | pass | OCI single | 1 | `samples/recipes/mailpit/` | ~8s | HTTP 200 | SMTP capture UI |
| pocketbase | Developer tools | pass | OCI single | 1 | `samples/recipes/pocketbase/` | ~12s | HTTP 200 | admin at `/_/` |
| filebrowser | Developer tools | pass | OCI single | 1 | `samples/recipes/filebrowser/` | ~10s | HTTP 200 | rootless port config |
| actual | Productivity | pass | OCI single | 1 | `samples/recipes/actual/` | ~15s | HTTP 200 | local-first finance |
| homepage | Productivity | pass | OCI single | 1 | `samples/recipes/homepage/` | ~25s | HTTP 200 | config-driven dashboard |
| stirling-pdf | Productivity | pass | OCI single | 1 | `samples/recipes/stirling-pdf/` | ~30s | HTTP 200 | readiness at `/login` |
| pgweb | Developer tools | pass | OCI single | 1 | `samples/recipes/pgweb/` | ~30s | HTTP 200 | standalone, no DB |
| shiori | Productivity | pass | OCI single | 1 | `samples/recipes/shiori/` | ~15s | HTTP 200 | SQLite, fast |
| lobe-chat | AI / LLM apps | pass | OCI single | 1 | `samples/recipes/lobe-chat/` | ~70s | HTTP 307→200 | lite mode |
| linkwarden | Productivity | pass | OCI multi | 3 | `samples/recipes/linkwarden/` | ~25s cached | HTTP 200 | postgres+meilisearch+app |
| adminer | Developer tools | pass | OCI single | 1 | `samples/recipes/adminer/` | ~10s | HTTP 200 | lightweight DB admin |
| changedetection | Monitoring / utilities | pass | OCI single | 1 | `samples/recipes/changedetection/` | ~76s | HTTP 200 | change monitoring |
| bytebase | Developer tools | pass | OCI single | 1 | `samples/recipes/bytebase/` | ~141s | HTTP 200 | first-run DB init |
| dbgate | Developer tools | pass | OCI single | 1 | `samples/recipes/dbgate/` | ~171s | HTTP 200 | large image |
| excalidraw | Productivity | pass | OCI single | 1 | `samples/recipes/excalidraw/` | ~60s | HTTP 200 | static SPA |
| pingvin-share | Productivity | pass | OCI single | 1 | `samples/recipes/pingvin-share/` | ~150s | HTTP 200 | file sharing |
| kavita | Monitoring / utilities | pass | OCI single | 1 | `samples/recipes/kavita/` | ~151s | HTTP 200 | ebook server |
| searxng | Monitoring / utilities | pass | OCI single | 1 | `samples/recipes/searxng/` | ~50s | HTTP 200 | metasearch |
| grist | Data / internal tools | pass | OCI single | 1 | `samples/recipes/grist/` | ~136s | HTTP 200 | spreadsheet DB |
| wallabag | Productivity | pass | OCI single | 1 | `samples/recipes/wallabag/` | ~70s | HTTP 200 | read-it-later |
| flowise | AI / LLM apps | pass | OCI single | 1 | `samples/recipes/flowise/` | ~15s | HTTP 200 | AI workflow builder |
| litellm | AI / LLM apps | pass | OCI single | 1 | `samples/recipes/litellm/` | ~212s | HTTP 200 | LLM proxy |
| directus | Developer tools | pass | OCI single | 1 | `samples/recipes/directus/` | ~15s | HTTP 302→200 | headless CMS |
| anything-llm | AI / LLM apps | pass | OCI single | 1 | `samples/recipes/anything-llm/` | ~300s | HTTP 200 | RAG AI workspace |
| superset | Data / internal tools | pass | OCI single | 1 | `samples/recipes/superset/` | ~240s | HTTP 200 | BI platform |
| langflow | AI / LLM apps | pass | OCI single | 1 | `samples/recipes/langflow/` | ~360s | HTTP 200 | needs 420s timeout |
| langfuse | AI / LLM apps | pass | OCI multi | 2 | `samples/recipes/langfuse/` | ~120s | HTTP 200 | LLM observability |
| umami | Monitoring / utilities | pass | OCI multi | 2 | `samples/recipes/umami/` | ~90s | HTTP 200 | analytics |
| paperless-ngx | Productivity | pass | OCI multi | 3 | `samples/recipes/paperless-ngx/` | ~90s | HTTP 302→200 | document manager |
| outline | Productivity | pass | OCI multi | 3 | `samples/recipes/outline/` | ~90s | HTTP 200 | wiki |
| vikunja | Productivity | partial | OCI single | 1 | `samples/recipes/vikunja/` | >300s | — | readiness timeout; first-run migration |
| promptfoo | Developer tools | partial | OCI single | 1 | `samples/recipes/promptfoo/` | — | — | needs eval data CLI args |
| photoprism | Monitoring / utilities | partial | OCI single | 1 | `samples/recipes/photoprism/` | >180s pull | — | >3 GB image cold pull |
| mindsdb | Data / internal tools | partial | OCI single | 1 | `samples/recipes/mindsdb/` | >240s pull | — | >5 GB image cold pull |
| dify | AI / LLM apps | partial | OCI multi | 6 | `samples/recipes/dify/` | ~15 min cold | HTTP 200 | 10-service stack deferred |
| logto | Developer tools | blocked | OCI multi | 2 | `samples/recipes/logto/` | — | — | multi-step DB init |
| librechat | AI / LLM apps | blocked | OCI multi | 2 | `samples/recipes/librechat/` | — | — | MongoDB sidecar |
| twenty | Productivity | blocked | OCI multi | 3 | `samples/recipes/twenty/` | — | — | private image (403) |
| ToolJet | Developer tools | blocked | OCI multi | — | — | — | — | needs runtime deps |
| Budibase | Developer tools | blocked | OCI multi | — | — | — | — | needs runtime deps |
| Appsmith | Developer tools | blocked | OCI multi | — | — | — | — | needs runtime deps |
| Penpot | Developer tools | blocked | OCI multi | — | — | — | — | needs runtime deps |
| Standard Notes | Productivity | blocked | OCI multi | — | — | — | — | needs runtime deps |
| Redash | Data / internal tools | blocked | OCI multi | — | — | — | — | needs runtime deps |
| Zep | AI / LLM apps | blocked | OCI multi | — | — | — | — | needs runtime deps |
| LlamaIndex Deploy | AI / LLM apps | blocked | OCI multi | — | — | — | — | needs runtime deps |
| Ragflow | AI / LLM apps | blocked | OCI multi | — | — | — | — | GPU + multi-service |

---

## Grouping Sections

### AI / LLM Apps (10 pass + 4 partial + 1 blocked + 2 deferred)

Apps ready to demo: **memos**, **n8n** (single-user), **lobe-chat** (lite), **flowise**, **litellm**, **anything-llm**, **langflow**, **langfuse**, **open-webui** (partial — slow), **dify** (partial-pass, heavy).

| App | Status | Notes |
|---|---|---|
| memos | pass | Fastest AI-adjacent app |
| n8n | pass | Workflow automation |
| lobe-chat | pass | Multi-LLM chat UI |
| flowise | pass | Drag-and-drop AI workflows |
| litellm | pass | LLM proxy gateway |
| anything-llm | pass | RAG workspace |
| langflow | pass | Visual LangChain workflows |
| langfuse | pass | LLM observability (emulated) |
| open-webui | partial | HF model downloads on first run |
| dify | partial | 15 min cold startup; heavy stack |
| librechat | blocked | MongoDB sidecar |
| Zep | blocked | Multi-service |
| LlamaIndex Deploy | blocked | Multi-service |
| Ragflow | blocked | GPU + multi-service |

### Productivity (7 pass + 1 partial + 1 blocked)

| App | Status | Notes |
|---|---|---|
| actual | pass | Local-first finance |
| homepage | pass | Personal dashboard |
| stirling-pdf | pass | PDF tool suite |
| shiori | pass | Bookmark manager |
| linkwarden | pass | Bookmark archive (3 services) |
| wallabag | pass | Read-it-later |
| paperless-ngx | pass | Document manager (3 services) |
| outline | pass | Wiki (3 services) |
| vikunja | partial | First-run migration slow |
| twenty | blocked | Private image |
| Standard Notes | blocked | Multi-service |

### Developer Tools (9 pass + 1 partial + 1 blocked)

| App | Status | Notes |
|---|---|---|
| mailpit | pass | SMTP capture |
| pocketbase | pass | Backend + admin UI |
| filebrowser | pass | File manager |
| pgweb | pass | Postgres web client |
| adminer | pass | DB management |
| bytebase | pass | Database CI/CD |
| dbgate | pass | Universal DB client |
| directus | pass | Headless CMS |
| promptfoo | partial | Needs eval CLI args |
| logto | blocked | Multi-step DB init |
| ToolJet | blocked | Multi-service |
| Budibase | blocked | Multi-service |
| Appsmith | blocked | Multi-service |
| Penpot | blocked | Multi-service |

### Data / Internal Tools (4 pass + 1 partial)

| App | Status | Notes |
|---|---|---|
| nocodb | pass | Airtable alternative |
| grist | pass | Spreadsheet DB |
| superset | pass | BI platform |
| mindsdb | partial | >5 GB image |
| Redash | blocked | Multi-service |

### Monitoring / Utilities (4 pass + 1 partial)

| App | Status | Notes |
|---|---|---|
| uptime-kuma | pass | Uptime monitoring |
| changedetection | pass | Website change monitor |
| kavita | pass | Ebook server |
| searxng | pass | Metasearch engine |
| umami | pass | Analytics (2 services) |
| photoprism | partial | >3 GB cold pull |

### Heavy or Unsafe Deferred Apps

The following are intentionally deferred from the initial catalog:

| App | Reason | Suggested path |
|---|---|---|
| Dify (full) | 10-service stack; needs one-shot init, nginx, sandbox | Wait for runtime: run_once, ingress |
| Photoprism | >3 GB image; needs originals/storage state split | Slim recipe + AODD with timeout |
| MindsDB | >5 GB image; needs slim server variant | Use cloud/server image |
| Supabase | 15+ services, kong gateway | Out of scope for OCI catalog |
| PostHog | ClickHouse + Kafka concurrency | Out of scope for initial catalog |
| Immich | ML services + multi-service | Out of scope for initial catalog |
| GPU-only tools | CUDA required | Out of scope until GPU pass-through |

---

## Summary by Runtime Shape

| Shape | Count | Examples |
|---|---|---|
| OCI single | 31 | memos, n8n, flowise, ... |
| OCI multi (2 services) | 4 | langfuse, umami, logto, librechat |
| OCI multi (3+ services) | 6 | linkwarden, paperless-ngx, outline, twenty, dify, ragflow |

## Current Catalog Status

### What is ready to demo

- 34 pass-status recipes can be demoed with `ato run samples/recipes/<app>`
- AI showcase lane: flowise, langflow, anything-llm, n8n, lobe-chat, litellm
- Multi-service demos: linkwarden, langfuse, umami, paperless-ngx, outline
- All single-service apps start within 5 minutes (most < 60s)

### What is experimental

- **dify** — partial-pass; works but 15-min cold startup, no file upload verification, no nginx routing
- **open-webui** — partial; first-run HuggingFace model downloads take 3-4 min
- **langflow** — pass but 360s startup; needs 420s readiness timeout
- **vikunja** — partial; first-run DB migration may timeout
- **superset** — pass but 240s startup; SQLite dev mode only

### What is unsafe/deferred

- Docker socket apps (OpenHands, coolify, portainer, dozzle, dokploy) — **rejected**
- GPU-only tools (AUTOMATIC1111, ComfyUI, InvokeAI, h2oGPT) — **rejected**
- Heavy stacks (Supabase, PostHog, Immich) — **deferred**
- Private images (twenty) — **blocked** (upstream issue)
- Very large images (photoprism, mindsdb) — **partial** (solve with slim variants)

### What requires runtime work

- `run_once`/one-shot service support — unblocks init containers for Dify and similar
- Shared mutable state across services — unblocks worker/api storage sharing
- Ingress/proxy layer or static port mapping — unblocks Dify-like two-tier web/app architectures
- Pull timeout per target — unblocks cold-start-heavy apps without bumping global window
- Slim/alternative image variants for mindsdb, photoprism
