# Image Tag Pinning Audit

Audit of all recipe image tags under `samples/recipes/` for reproducibility.
Identifies `:latest`, `:main`, untagged refs, and rolling tags, then
proposes pinned stable version tags.

**Audit date:** 2026-05-24  
**Policy:** Prefer stable semantic version tags where available. If upstream
does not provide stable tags, keep rolling tag but document why. Do not pin
to digest in recipe unless project policy requires digest-only recipes.
Preserve AODD-time digest in docs for traceability.

---

## Required Focus Recipes

| Recipe | Current image | Tag type | Proposed pinned tag | AODD digest (2026-05-22/23) | Action |
|---|---|---|---|---|---|
| n8n | `n8nio/n8n:latest` | rolling | `n8nio/n8n:1.93.0` | `sha256:10bb1df216...4a556` | **pin proposed — not yet AODD-confirmed** |
| NocoDB | `nocodb/nocodb:latest` | rolling | `nocodb/nocodb:0.259.0` | `sha256:c62f33e8a28b...c467` | **pin proposed — not yet AODD-confirmed** |
| filebrowser | `filebrowser/filebrowser:latest` | rolling | `filebrowser/filebrowser:v2.32.0` | `sha256:9ffebe23dc98...` | **pin proposed — not yet AODD-confirmed** |
| actual | `actualbudget/actual-server:latest` | rolling | `actualbudget/actual-server:24.12.0` | `sha256:7228365ca65e...` | **pin proposed — not yet AODD-confirmed** |
| homepage | `ghcr.io/gethomepage/homepage:latest` | rolling | `ghcr.io/gethomepage/homepage:v1.2.1` | `sha256:8e5f595273f0...` | **pin proposed — not yet AODD-confirmed** |
| Open WebUI | `ghcr.io/open-webui/open-webui:main` | rolling branch | keep `:main` (no stable tag published) | `sha256:7d403dfa5efe...` | Keep rolling — upstream only publishes `:main`. Document reason. |
| Flowise | `flowiseai/flowise:2.2.7` | version | already pinned | — | No action |
| Langflow | `langflowai/langflow:1.1.4` | version | already pinned | — | No action |
| AnythingLLM | `mintplexlabs/anythingllm:1.7.6` | version | already pinned | — | No action |
| Dify | `langgenius/dify-api:1.14.2`, `langgenius/dify-web:1.14.2` | version | already pinned | — | No action |
| Langfuse | `langfuse/langfuse:2.32.0` | version | already pinned | — | No action |
| Linkwarden | `ghcr.io/linkwarden/linkwarden:v2.14.1` | version | already pinned | — | No action |

---

## All Recipes — Full Audit

| Recipe | Current image | Tag type | Stable tag available? | Action |
|---|---|---|---|---|
| uptime-kuma | `louislam/uptime-kuma:1` | major track | `:1` is effectively stable | Keep — major track is stable enough |
| memos | `neosmemo/memos:stable` | rolling stable | `stable` is the upstream stable track | Keep — upstream convention |
| n8n | `n8nio/n8n:latest` | rolling | yes — `n8nio/n8n:1.93.0` | Pin proposed |
| nocodb | `nocodb/nocodb:latest` | rolling | yes — `nocodb/nocodb:0.259.0` | Pin proposed |
| open-webui | `ghcr.io/open-webui/open-webui:main` | rolling branch | no — upstream only publishes `:main` | Keep rolling; document |
| mailpit | `axllent/mailpit:latest` | rolling | yes — `axllent/mailpit:1.22.0` | Pin proposed |
| pocketbase | `ghcr.io/muchobien/pocketbase:latest` | rolling | yes — `ghcr.io/muchobien/pocketbase:0.26.0` | Pin proposed |
| filebrowser | `filebrowser/filebrowser:latest` | rolling | yes — `filebrowser/filebrowser:v2.32.0` | Pin proposed |
| actual | `actualbudget/actual-server:latest` | rolling | yes — `actualbudget/actual-server:24.12.0` | Pin proposed |
| homepage | `ghcr.io/gethomepage/homepage:latest` | rolling | yes — `ghcr.io/gethomepage/homepage:v1.2.1` | Pin proposed |
| stirling-pdf | `frooodle/s-pdf:2.11.0` | version | already pinned | None |
| pgweb | `sosedoff/pgweb:0.17.0` | version | already pinned | None |
| shiori | `ghcr.io/go-shiori/shiori:v1.8.0` | version | already pinned | None |
| lobe-chat | `lobehub/lobe-chat:1.143.3` | version | already pinned | None |
| linkwarden | `ghcr.io/linkwarden/linkwarden:v2.14.1` | version | already pinned | None |
| adminer | `adminer:4.8.1` | version | already pinned | None |
| changedetection | `ghcr.io/dgtlmoon/changedetection.io:0.49.0` | version | already pinned | None |
| bytebase | `bytebase/bytebase:3.7.0` | version | already pinned | None |
| dbgate | `dbgate/dbgate:6.5.0` | version | already pinned | None |
| excalidraw | `excalidraw/excalidraw:latest` | rolling | yes — `excalidraw/excalidraw:0.17.6` | Pin proposed |
| pingvin-share | `stonith404/pingvin-share:latest` | rolling | yes — `stonith404/pingvin-share:1.6.0` | Pin proposed |
| kavita | `lscr.io/linuxserver/kavita:latest` | rolling | yes — `lscr.io/linuxserver/kavita:0.8.5` | Pin proposed |
| searxng | `searxng/searxng:latest` | rolling | yes — `searxng/searxng:2025.04.03` | Pin proposed |
| grist | `gristlabs/grist:1.5.1` | version | already pinned | None |
| wallabag | `wallabag/wallabag:2.6.10` | version | already pinned | None |
| flowise | `flowiseai/flowise:2.2.7` | version | already pinned | None |
| litellm | `ghcr.io/berriai/litellm:main-stable` | rolling stable | `main-stable` is upstream stable track | Keep rolling; document |
| directus | `directus/directus:11.8.0` | version | already pinned | None |
| anything-llm | `mintplexlabs/anythingllm:1.7.6` | version | already pinned | None |
| superset | `apache/superset:4.1.2` | version | already pinned | None |
| langflow | `langflowai/langflow:1.1.4` | version | already pinned | None |
| langfuse | `langfuse/langfuse:2.32.0` | version | already pinned | None |
| dify | `langgenius/dify-api:1.14.2`, `web:1.14.2` | version | already pinned | None |
| umami | `ghcr.io/umami-software/umami:postgresql-v2.17.0` | version | already pinned | None |
| paperless-ngx | `ghcr.io/paperless-ngx/paperless-ngx:2.13.5` | version | already pinned | None |
| outline | `outlinewiki/outline:0.82.0` | version | already pinned | None |
| vikunja | `vikunja/vikunja:0.24.6` | version | already pinned | None |
| promptfoo | `ghcr.io/promptfoo/promptfoo:latest` | rolling | yes — available | Pin proposed |
| photoprism | `photoprism/photoprism:231128-ce` | date-based | `231128-ce` is effectively a version | Keep |
| mindsdb | `mindsdb/mindsdb:v25.4.4.0` | version | already pinned | None |
| twenty | `twentyhq/twenty-server:0.50.0` | version | already pinned | None |
| logto | `svhd/logto:1.26.0` | version | already pinned | None |
| librechat | `ghcr.io/danny-avila/librechat:v0.7.8` | version | already pinned | None |

---

## Summary

| Classification | Count |
|---|---|
| Already pinned (version tag) | 25 |
| Rolling tag — keep (upstream convention) | 3 |
| Rolling tag — pin proposed | 11 |
| Rolling tag — keep (no stable tag) | 1 |

### Pin proposed recipes (not yet AODD-confirmed)

The following recipes have proposed pin changes. Each tagged version
was selected based on the AODD-resolved digest and upstream release
history. The changed tags need AODD re-confirmation before the recipe
can be marked pass.

Cutoff proposal: recipes with `:latest` → version that were in
Batch 1-2 scope and AODD-confirmed at those versions.

### Recipes where rolling tags are intentional

| Recipe | Reason |
|---|---|
| open-webui | Upstream only publishes `:main` — no stable semver tags |
| litellm | Upstream stable track is `:main-stable` |
| memos | `:stable` is upstream's stable track convention |
| uptime-kuma | `:1` major track is effectively version-pinned |
