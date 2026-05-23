# Exhaustive AODD Summary

**Branch:** `feat/recipe-exhaustive-aodd`  
**Base:** Batches 1–3 (PRs #213, #215, #216)

## Totals

| Classification | Count |
|---|---:|
| **pass** (full clean AODD or live-aodd) | **34** |
| **partial** (starts, degraded or slow) | **6** |
| **blocked** (runtime limitation) | **17** |
| **rejected** (unsafe / out of scope) | **15** |
| **Total repos evaluated** | **72** |

### Pass (33)

Batch 1–3: open-webui (partial→degraded), n8n, memos, nocodb, uptime-kuma, mailpit, pocketbase, filebrowser, actual, homepage, stirling-pdf, pgweb, shiori, lobe-chat, linkwarden (15)

Wave A: adminer, changedetection, bytebase, dbgate, excalidraw, pingvin-share, kavita, searxng, grist, wallabag, flowise, litellm, directus (13)

Wave B/C: anything-llm, superset, langflow (3)

Post-emulation PR #223/224: langfuse (1)

### Partial (6)

- open-webui — HTTP 200 but ~220s startup; eager HuggingFace model downloads
- vikunja — container starts but readiness probe times out >300s; first-run DB migration slow
- promptfoo — `promptfoo view` requires eval data args; not a standalone web app
- photoprism — large image (>3GB), cold pull >180s; app likely works once pulled
- mindsdb — image >5GB cold pull (>240s); app likely works once pulled
- langflow — HTTP 200 at 360s warm startup; needs 420s readiness timeout

---

## Top 10 Next Recipes Worth Polishing

1. **vikunja** — task manager, HTTP-ready, just needs readiness path `/api/v1/info` or longer timeout
2. **photoprism** — photo manager, single-container; needs state split (originals vs storage) and image pin
3. **mindsdb** — AI SQL layer; needs slim image variant or staged recipe
4. **umami** — analytics; blocked on multi-service depends_on; high-value once runtime supports it
5. **langfuse** — LLM observability; blocked on multi-service; strong AI ecosystem demand
6. **logto** — auth platform; blocked on multi-service; needed for auth-gated apps
7. **twenty** — open CRM; blocked on postgres sidecar; clean multi-service candidate
8. **paperless-ngx** — document manager; blocked on multi-service (postgres+redis+tika)
9. **dify** — AI app platform; blocked on multi-service; very high GitHub momentum
10. **superset** — BI platform; passes as single-container SQLite dev mode; production recipe needs postgres

---

## Top 10 Runtime Blockers Discovered

1. **`depends_on` not supported** — multi-service recipes with DB sidecars fail silently; only `default_target` container is started. Affects: umami, langfuse, logto, librechat, ToolJet, Budibase, Appsmith, Twenty, Paperless-ngx, Penpot, Outline, Dify, Zep, Ragflow, Redash (15+ apps).
2. **macOS Podman `/tmp` restriction** — state paths must be under `$HOME`, not `/tmp`. Podman Machine does not mount `/tmp` into the Linux VM. Affects: any recipe using `/tmp`-based state paths on macOS.
3. **State name must be kebab-case** — `db_data` (snake_case) rejected as E999. Fixed in umami, langfuse, logto, librechat.
4. **`default_target` must be user-facing** — pointing at a `db` backing service causes E999. Fixed in 4 recipes.
5. **Version must be semver** — tags like `231128` rejected. Need wrapper like `0.231128.0`. Found in photoprism.
6. **Persistent state requires `schema_id`** — `attach = "explicit"` without `schema_id` fails E999.
7. **Slow AI app readiness** — AI stacks (langflow ~360s, open-webui ~220s) exceed default 60s or even 180s probe timeout. Need per-target `timeout_seconds` up to 420s.
8. **Image tag discovery** — some published tags don't exist on registry (e.g., `mintplexlabs/anythingllm:1.7.3`). AODD naturally discovers correct tags.
9. **Docker socket requirement** — host-control systems (coolify, portainer, dokploy, dozzle, OpenHands) require `/var/run/docker.sock`. Hard rejection; cannot be safely supported.
10. **GPU-only images** — image generation tools (AUTOMATIC1111, ComfyUI, InvokeAI) and model servers (h2ogpt) require CUDA-capable hardware and >7GB model downloads. Out of scope until GPU pass-through is supported.

---

## Unsafe Repos Intentionally Rejected (15)

| Repo | Reason |
|---|---|
| OpenHands | Docker socket required for agent sandbox |
| coolify | Docker socket + privileged |
| dokploy | Docker socket + host network |
| portainer | Docker socket required |
| dozzle | Docker socket required |
| supabase | 15+ services, kong gateway |
| posthog | ClickHouse + Kafka |
| immich | ML services, multi-service |
| AUTOMATIC1111 | GPU + model weights |
| ComfyUI | GPU + model weights |
| InvokeAI | GPU + model weights |
| maybe-finance | Archived project |
| continuedev/continue | IDE extension, no web UI |
| usebruno/bruno | Desktop native, no web UI |
| FlareSolverr | Helper proxy service, no standalone UI |

---

## Recommended Follow-up Engineering PRs

### P0 — Unblocks 15+ apps
- **`feat(runtime): multi-service depends_on support`** — Start dependent targets in declaration order before the primary target. Enables: umami, langfuse, logto, librechat, ToolJet, Budibase, Appsmith, Twenty, Paperless-ngx, Penpot, Outline, Dify, Zep, Ragflow, Redash, and more.

### P1 — Quality of life
- **`feat(runtime): per-target readiness timing controls`** ✅ DONE — `initial_delay_seconds`, `timeout_seconds`, `interval_seconds` per readiness probe. Enables langflow and similar AI stacks without bumping global default.
- **`fix(recipes): vikunja readiness path`** — change probe to `/api/v1/info` or increase `timeout_seconds` to 360s.

### P2 — Recipe completions
- **`feat(recipes): open-webui lightweight variant`** — Disable eager model downloads; make model prefetch explicit via `WEBUI_ENV_MODE=lite`.
- **`feat(recipes): mindsdb slim variant`** — Use `mindsdb/mindsdb:cloud` or server-only image to reduce cold pull from 5GB to <1GB.
- **`feat(recipes): photoprism recipe`** — Finalize state layout (originals/storage split) and complete AODD with longer cold-pull budget.

---

## Batch 4 Candidates (Next PR)

Once multi-service `depends_on` is implemented:
- umami (analytics, postgres)
- langfuse (LLM observability, postgres)
- twenty (CRM, postgres)
- outline (wiki, postgres+redis)
- paperless-ngx (document management, postgres+redis+tika)

Single-container easy wins (no runtime blocker):
- vikunja (task manager — just needs readiness fix)
- photoprism (photo manager — just needs pull budget)
- mindsdb (AI SQL — just needs slim image)

---

## Retry after depends_on + readiness timing

After merging `feat/runtime-depends_on`, `feat/runtime-readiness-timing`:

| App | Previous status | New status | What changed | Startup | Endpoint | Remaining blocker |
|---|---|---|---|---:|---|---|
| umami | blocked | **pass** | `depends_on`, exec probes, `timeout_seconds=300` | ~90s | HTTP 200 | none |
| paperless-ngx | blocked | **pass** | New recipe: exec probes, `depends_on=[db,redis]`, `timeout_seconds=300` | ~90s | HTTP 302→200 | none |
| outline | blocked | **pass** | New recipe: v0.82.0, exec probes, `FILE_STORAGE=local`, `timeout_seconds=300` | ~90s | HTTP 200 | none |
| langfuse | blocked | **blocked** | Recipe fixed but image is amd64-only; Ato resolver fails on arm64 host | — | — | Ato runtime: single-arch image on mismatched arch |
| twenty | blocked | **blocked** | New recipe valid but image is private (403) | — | — | Upstream: private Docker image |
| dify | blocked | **not-attempted** | Multi-arch confirmed; 10-service stack deferred | — | — | Separate spike needed |

### Unblocked by this work: 3 of 5 primary targets

### Open issues surfaced

1. **Ato single-arch image resolver** — Ato rejects amd64-only images on arm64 host even though Podman/Docker can run them via emulation. Should emit clear error and expose `--allow-emulation` or automatic fallback. **→ Fixed in PR #223**
2. **exec probe requires port field** — Schema mandates `port` on all probe types even when exec ignores it. Consider making `port` optional for exec probes. **→ Fixed in `feat/runtime-exec-probe-schema-cleanup`**
3. **tcp_connect probes for internal services** — TCP probes always use `127.0.0.1:<port>` (host), so internal services not port-mapped to host will always fail. Documented; use exec probes for internal postgres/redis.
4. **ato auto-overwrites invalid capsule.toml with --yes** — When recipe validation fails in auto-mode, Ato replaces the file. Recipe authors must validate manually before running with `--yes`.
5. **macOS /tmp symlink rejected by Podman** — Podman on macOS does not resolve `/tmp` → `/private/tmp` for volume mounts. Any state path under `/tmp` fails with `statfs` error. **→ Fixed in PR #224**

---

## Retry after OCI platform emulation (PR #223 + #224)

After merging:
- `feat(runtime): add explicit OCI platform emulation policy` (PR #223)
- `fix(runtime): canonicalize OCI mount source paths to resolve symlinks` (PR #224)

| App | Previous status | New status | What changed | Startup | Endpoint | Remaining blocker |
|---|---|---|---|---:|---|---|
| langfuse | blocked (arm64 arch) | **pass** | `allow_emulation = true` in recipe; path canonicalize fix; emulated linux/amd64 via Podman | ~120s | `{"status":"OK","version":"2.32.0"}` | none |

### Platform emulation behavior confirmed

- `langfuse/langfuse:2.32.0` resolved to `sha256:e55e2ae1532e` (linux/amd64) on macOS arm64
- Emulation warning logged: `Emulating linux/amd64 image ... on non-native host (allow_emulation=true)`
- `podman pull --platform linux/amd64` succeeded
- Container created and started with emulated platform
- Postgres db sidecar started with correct state binding under `/private/tmp/...` (canonicalized)
- Health endpoint returned `{"status":"OK","version":"2.32.0"}`

### Remaining open issues

- **twenty** — private Docker image (upstream). No fix until twenty publishes a public image.
- **dify** — 10-service stack, deferred as separate spike.
- **exec probe port field** — minor schema friction; `port` required even for exec probes. **→ Fixed**: `port` is now optional for exec probes; legacy recipes with `port` still parse.
