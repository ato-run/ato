# Exhaustive AODD Matrix

All researched self-hosted app repositories tested against Ato OCI recipe infrastructure.

**Branch:** `feat/recipe-exhaustive-aodd`  
**Base:** post-merge of PRs #212, #213, #215, #216 (Batches 1–3)  
**Test policy:** Clean `ATO_HOME` per repo via `mktemp -d "$HOME/.ato-exhaust-XXXXXX"`  
**Host:** macOS arm64, Podman Machine  

## Status Definitions

| Status | Meaning |
|---|---|
| **pass** | Clean ATO_HOME, HTTP ready, `ato ps` confirmed, `ato stop --all` clean |
| **partial** | App starts but degraded, slow, probe mismatch, or manual step needed |
| **blocked** | Should be supported but current Ato runtime/recipe capability is missing |
| **rejected** | Unsafe, requires Docker socket / privileged / GPU / host network |

## Attempt Types

| Type | Meaning |
|---|---|
| **live-aodd** | Full ATO_HOME clean run, readiness probe, `ato ps`, `ato stop --all` |
| **lightweight-smoke** | Pull + container start verification without full AODD cycle |
| **static-safety-review** | No execution; classified from manifest/docs inspection |
| **skipped-known-risk** | In scope but deprioritized this cycle; no test run |

---

## Already Validated — Batches 1–3

| App | Repo | Status | Startup | Endpoint | Notes |
|---|---|---|---:|---|---|
| Open WebUI | open-webui/open-webui | **partial** | ~220s | HTTP 200 | Eager HuggingFace model downloads on first run |
| n8n | n8n-io/n8n | **pass** | 45s | HTTP 200 | Single-user/demo mode; pin image tag |
| Memos | usememos/memos | **pass** | 20s | HTTP 200 | |
| NocoDB | nocodb/nocodb | **pass** | 35s | HTTP 200 | Pin image tag |
| Uptime Kuma | louislam/uptime-kuma | **pass** | 18s | HTTP 200 | |
| Mailpit | axllent/mailpit | **pass** | 8s | HTTP 200 | SMTP 1025 + HTTP 8025 |
| Pocketbase | pocketbase/pocketbase | **pass** | 12s | HTTP 200 | Readiness at `/_/` |
| Filebrowser | filebrowser/filebrowser | **pass** | 10s | HTTP 200 | rootless: FB_PORT=8080 |
| Actual Budget | actualbudget/actual | **pass** | 15s | HTTP 200 | |
| Homepage | gethomepage/homepage | **pass** | 25s | HTTP 200 | |
| Stirling PDF | Stirling-Tools/Stirling-PDF | **pass** | 30s | HTTP 200 | Readiness at `/login` |
| pgweb | sosedoff/pgweb | **pass** | 30s | HTTP 200 | Standalone, no DB required |
| Shiori | go-shiori/shiori | **pass** | 15s | HTTP 200 | |
| Lobe Chat | lobehub/lobe-chat | **pass** | 70s | HTTP 307 | Redirect = ready |
| Linkwarden | linkwarden/linkwarden | **pass** | 25s cached | HTTP 200 | Cold pull ~4 min; postgres+meilisearch+app |

---

## Wave A — Easy / Single Container

| App | Repo | Attempt | Status | Startup | Endpoint | Main blocker | Follow-up |
|---|---|---|---|---:|---|---|---|
| adminer | vrana/adminer | live-aodd | **pass** | 10s | HTTP 200 | — | — |
| changedetection.io | changedetection-io/changedetection.io | live-aodd | **pass** | 76s | HTTP 200 | — | — |
| bytebase | bytebase/bytebase | live-aodd | **pass** | 141s | HTTP 200 | — | First-run DB init |
| dbgate | dbgate/dbgate | live-aodd | **pass** | 171s | HTTP 200 | — | Large image |
| excalidraw | excalidraw/excalidraw | live-aodd | **pass** | 60s | HTTP 200 | — | Static SPA |
| pingvin-share | stonith404/pingvin-share | live-aodd | **pass** | 150s | HTTP 200 | — | Use `latest` tag |
| kavita | Kareadita/Kavita | live-aodd | **pass** | 151s | HTTP 200 | — | Use linuxserver image |
| searxng | searxng/searxng | live-aodd | **pass** | 50s | HTTP 200 | — | Needs `SEARXNG_SECRET` |
| grist | gristlabs/grist-core | live-aodd | **pass** | 136s | HTTP 200 | — | Needs session secret |
| wallabag | wallabag/wallabag | live-aodd | **pass** | 70s | HTTP 200 | — | SQLite mode |
| Flowise | FlowiseAI/Flowise | live-aodd | **pass** | 15s | HTTP 200 | — | AI workflow builder |
| LiteLLM | BerriAI/litellm | live-aodd | **pass** | 212s | HTTP 200 | — | `main-stable` tag |
| directus | directus/directus | live-aodd | **pass** | 15s | HTTP 302 | — | SQLite mode; state must be under $HOME |
| vikunja | go-vikunja/vikunja | live-aodd | **partial** | >300s | — | Readiness timeout; first-run migration | Increase timeout or add `/health` probe |
| promptfoo | promptfoo/promptfoo | live-aodd | **partial** | — | — | Needs eval data CLI arg | `promptfoo view` requires `--` eval args |

---

## Wave B — Developer / Data Tools

| App | Repo | Attempt | Status | Startup | Endpoint | Main blocker | Follow-up |
|---|---|---|---|---:|---|---|---|
| AnythingLLM | Mintplex-Labs/anything-llm | live-aodd | **pass** | ~300s cold pull | HTTP 200 | — | 2.79GB image |
| superset | apache/superset | live-aodd | **pass** | 240s | HTTP 200 | — | Single-container SQLite dev mode |
| photoprism | photoprism/photoprism | lightweight-smoke | **partial** | >180s pull | — | Large image (>3GB cold pull) | Pin tag; split originals/storage state |
| umami | umami-software/umami | live-aodd | **blocked** | — | — | `depends_on` not supported; db sidecar not started | Runtime: multi-service depends_on support |
| langfuse | langfuse/langfuse | static-safety-review | **blocked** | — | — | postgres sidecar required | Runtime: multi-service depends_on support |
| logto | logto-io/logto | static-safety-review | **blocked** | — | — | postgres sidecar required | Runtime: multi-service depends_on support |
| LibreChat | danny-avila/LibreChat | static-safety-review | **blocked** | — | — | MongoDB sidecar required | Runtime: multi-service depends_on support |
| ToolJet | ToolJet/ToolJet | static-safety-review | **blocked** | — | — | postgres sidecar required | Runtime: multi-service depends_on support |
| Budibase | Budibase/budibase | static-safety-review | **blocked** | — | — | multi-service (couch, redis, minio) | Runtime: multi-service depends_on support |
| Appsmith | appsmithorg/appsmith | static-safety-review | **blocked** | — | — | MongoDB sidecar required | Runtime: multi-service depends_on support |
| Twenty CRM | twentyhq/twenty | static-safety-review | **blocked** | — | — | postgres + redis sidecar | Runtime: multi-service depends_on support |
| Paperless-ngx | paperless-ngx/paperless-ngx | static-safety-review | **blocked** | — | — | postgres + redis + tika | Runtime: multi-service depends_on support |
| Penpot | penpot/penpot | static-safety-review | **blocked** | — | — | postgres + redis + exporter sidecars | Runtime: multi-service depends_on support |
| Outline | outline/outline | static-safety-review | **blocked** | — | — | postgres + redis sidecar | Runtime: multi-service depends_on support |
| Standard Notes | standardnotes/app | static-safety-review | **blocked** | — | — | multi-service + auth complexity | Runtime: multi-service depends_on support |
| Redash | getredash/redash | static-safety-review | **blocked** | — | — | postgres + redis + celery workers | Runtime: multi-service depends_on support |

---

## Wave C — AI / LLM Stack

| App | Repo | Attempt | Status | Startup | Endpoint | Main blocker | Follow-up |
|---|---|---|---|---:|---|---|---|
| Langflow | langflow-ai/langflow | live-aodd | **pass** | 360s warm | HTTP 200 | — | Heavy Python import; 420s timeout needed |
| MindsDB | mindsdb/mindsdb | lightweight-smoke | **partial** | >240s pull | — | Very large image (>5GB cold pull) | Use slim/server variant; split AI plugins |
| Dify | langgenius/dify | static-safety-review | **blocked** | — | — | postgres + redis + weaviate + sandbox sidecar | Runtime: multi-service depends_on support |
| Zep | getzep/zep | static-safety-review | **blocked** | — | — | postgres + neo4j sidecars | Runtime: multi-service depends_on support |
| LlamaIndex Deploy | run-llama/llama_deploy | static-safety-review | **blocked** | — | — | multi-service (message queue, control plane) | Runtime: multi-service depends_on support |
| Ragflow | infiniflow/ragflow | static-safety-review | **blocked** | — | — | elasticsearch + mysql + redis; GPU recommended | Runtime: multi-service + GPU pass-through |
| h2oGPT | h2oai/h2ogpt | static-safety-review | **rejected** | — | — | GPU/model-weight required (>7GB download before UI) | GPU pass-through out of scope |

---

## Wave D — Static Rejection / Unsafe

| App | Repo | Attempt | Reason | Notes |
|---|---|---|---|---|
| OpenHands | All-Hands-AI/OpenHands | static-safety-review | `/var/run/docker.sock` required for agent sandbox | Cannot remove socket requirement |
| Coolify | coollabsio/coolify | static-safety-review | `/var/run/docker.sock` + privileged mode | Host-control system |
| Dokploy | Dokploy/dokploy | static-safety-review | `/var/run/docker.sock` + host network | Host-control system |
| Portainer | portainer/portainer | static-safety-review | `/var/run/docker.sock` required | Docker management requires socket |
| Dozzle | amir20/dozzle | static-safety-review | `/var/run/docker.sock` required | Docker log viewer requires socket |
| Supabase | supabase/supabase | static-safety-review | 15+ service stack; kong gateway required | Multi-service too complex for initial catalog |
| PostHog | PostHog/posthog | static-safety-review | ClickHouse + Kafka + Celery workers | Multi-service too heavy |
| Immich | immich-app/immich | static-safety-review | ML services + multi-service (postgres + redis + typesense + machine-learning) | Heavy; ML/photo indexing |
| AUTOMATIC1111 | AUTOMATIC1111/stable-diffusion-webui | static-safety-review | GPU/CUDA required for meaningful use | Model weights >7GB |
| ComfyUI | Comfy-Org/ComfyUI | static-safety-review | GPU required; model download mandatory | Out of scope without GPU pass-through |
| InvokeAI | invoke-ai/InvokeAI | static-safety-review | GPU required; model download mandatory | Out of scope without GPU pass-through |
| Maybe Finance | maybe-finance/maybe | static-safety-review | Archived / inactive | Not maintained |
| Continue Dev | continuedev/continue | static-safety-review | IDE extension; no standalone web UI | Not a web catalog app |
| Bruno | usebruno/bruno | static-safety-review | Desktop-native app; no web server | Not a web catalog app |
| FlareSolverr | FlareSolverr/FlareSolverr | static-safety-review | Helper proxy service; no standalone user UI | Not useful as standalone catalog entry |

---

## Retry — After depends_on + exec readiness + per-target timing

Retry set after merging: `feat/runtime-depends_on`, `feat/runtime-readiness-timing`.

| App | Previous status | New status | What changed | Startup | Endpoint | Remaining blocker |
|---|---|---|---|---:|---|---|
| umami | blocked (no depends_on) | **pass** | Recipe: `depends_on=[db,redis]`, exec probes, `timeout_seconds=300` | ~90s | HTTP 200 | none |
| paperless-ngx | blocked (no depends_on) | **pass** | New recipe: 3 services, exec probes (`pg_isready`, `redis-cli ping`), `timeout_seconds=300` | ~90s | HTTP 302→200 | none |
| outline | blocked (no depends_on) | **pass** | New recipe: v0.82.0 (auto-migrates), `FILE_STORAGE=local`, exec probes, `timeout_seconds=300` | ~60–90s | HTTP 200 | none |
| langfuse | blocked (no depends_on) | **blocked** | Recipe fixed (localhost→db, exec probe, version 2.32.0) but image is amd64-only — Ato resolver fails on arm64 | — | — | Ato runtime: single-arch image resolver on arm64 host |
| twenty | blocked (no depends_on) | **blocked** | New recipe created (exec probes, depends_on correct) but image is private (403 Forbidden) | — | — | Upstream: private Docker image |
| dify | blocked (no depends_on) | **not-attempted** | Multi-arch confirmed (arm64+amd64); 10+ services including sandbox/plugin_daemon exceeds scope | — | — | Deferred: separate spike needed for 10-service stack |

### Key findings

- `depends_on` + exec readiness unblocked 3 of 5 primary targets (umami, paperless-ngx, outline)
- `tcp_connect` probes for internal (non-published) services always fail: internal ports aren't mapped to host. Use `exec` probes for postgres/redis.
- `exec` probe still requires the `port` field in the current schema (field is accepted but unused by exec path).
- Langfuse blocked by single-arch image — separate Ato issue: resolver should emit clear error and optionally allow emulation fallback.
- Twenty blocked by upstream private image — not an Ato issue.
- Dify is multi-arch capable but 10-service topology warrants its own spike.
