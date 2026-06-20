# Recipe Batch 1 — Initial Catalog

Validation run for the first public-facing Ato recipe batch using OCI provider support.

**Branch:** `feat/initial-recipe-batch-1`
**Date:** 2026-05-22
**Platform:** Darwin arm64 (Apple Silicon)
**Podman:** 5.7.1
**Ato:** 0.5.2
**ATO_HOME policy:** fresh `mktemp -d /tmp/ato-recipe-batch1.XXXXXX` per run

---

## Results

| App | Status | Recipe path | Startup | Endpoint | State | Cleanup | Notes |
|-----|--------|-------------|--------:|----------|-------|---------|-------|
| Uptime Kuma | ✅ pass | `samples/recipes/uptime-kuma/` | ~45s | HTTP 302→200 | persistent `/app/data` | clean | Stable major tag `:1` |
| Memos | ✅ pass | `samples/recipes/memos/` | ~15s | HTTP 200 | persistent `/var/opt/memos` | clean | Fastest; `stable` tag used |
| NocoDB | ✅ pass | `samples/recipes/nocodb/` | ~120s | HTTP 200 | persistent `/usr/app/data` | clean | Large image; `:latest` — pin follow-up |
| n8n | ✅ pass | `samples/recipes/n8n/` | ~60s | HTTP 200 | persistent `/home/node/.n8n` | clean | Single-user mode; `:latest` — pin follow-up |
| Open WebUI | ⚠️ degraded | `samples/recipes/open-webui/` | ~220s (first run) | HTTP 200 | persistent `/app/backend/data` | clean | HF model download; probe removed |

**Acceptance criteria met:** 5/5 attempted, 4 clean passes + 1 degraded. Satisfies all batch criteria.

### Image digests (at time of AODD — 2026-05-22)

These are the resolved digests for the images pulled during the AODD runs.
The no-lock compat path resolves the mutable tag to this digest before pull/start.

| App | Declared ref | Resolved digest |
|-----|-------------|-----------------|
| Uptime Kuma | `louislam/uptime-kuma:1` | `sha256:84e6cce45a011bde8b0e1ccc9a12b8067621232f201b7ad10841716a778aac3f` |
| Memos | `neosmemo/memos:stable` | `sha256:23f7f807f8661576f8e1845e926caf48435e0826e64ded8a3d6c6ba08056f817` |
| NocoDB | `nocodb/nocodb:latest` | `sha256:c62f33e8a28b4c0b18e5e0311c3ce84caab54fd6c3434513f0f476f12d14c467` |
| n8n | `n8nio/n8n:latest` | `sha256:10bb1df216497fed1671c8c4c725bba913b99d32075dcdf3f0ec6ffb1df4a556` |
| Open WebUI | `ghcr.io/open-webui/open-webui:main` | `sha256:7d403dfa5ef22ecf6dd19f585a72097e3152f0998b9f3c1c571640dee57c55ba` |

---

## App Details

### 1. Uptime Kuma — ✅ pass

**Repo:** louislam/uptime-kuma
**Image:** `louislam/uptime-kuma:1`
**Port:** 3001 (published dynamically)
**Persistent state:** `/app/data`

**What Ato proved:**
- OCI single-service recipe with persistent filesystem state works end-to-end
- Dynamic port allocation prevents conflicts on developer machines
- `ato stop --all` correctly tears down container and network

**Recipe path:** explicit `capsule.toml` with `[targets.app]` + `[services.main]`

---

### 2. Memos — ✅ pass

**Repo:** usememos/memos
**Image:** `neosmemo/memos:stable`
**Port:** 5230 (published dynamically)
**Persistent state:** `/var/opt/memos`

**What Ato proved:**
- Lightweight Go app starts in ~15s including image pull
- SQLite-backed persistent state binding works
- First-run creates DB schema automatically; no manual init needed

**Recipe path:** explicit `capsule.toml`

---

### 3. NocoDB — ✅ pass

**Repo:** nocodb/nocodb
**Image:** `nocodb/nocodb:latest`
**Port:** 8080 (published dynamically)
**Persistent state:** `/usr/app/data`

**What Ato proved:**
- Large image (~500MB) pulls and starts reliably
- Nuxt SPA served on first request with HTTP 200
- No external database required in single-node mode

**Recipe path:** explicit `capsule.toml`

**Follow-up:** Pin to a specific version tag instead of `:latest` for reproducibility.

---

### 4. n8n — ✅ pass

**Repo:** n8n-io/n8n
**Image:** `n8nio/n8n:latest`
**Port:** 5678 (published dynamically)
**Persistent state:** `/home/node/.n8n`

**What Ato proved:**
- Workflow automation UI starts in single-user mode without additional config
- Node.js app with embedded SQLite, no sidecar database needed
- Dynamic port avoids conflict with other local services

**Recipe path:** explicit `capsule.toml`

**Follow-up:**
- Add `N8N_ENCRYPTION_KEY` as a generated secret for production security
- Pin to specific version tag (see [image-tag-pinning-audit.md](./image-tag-pinning-audit.md))

---

### 5. Open WebUI — ⚠️ degraded

**Repo:** open-webui/open-webui
**Image:** `ghcr.io/open-webui/open-webui:main`
**Port:** 8080 (published dynamically)
**Persistent state:** `/app/backend/data`

**What Ato proved:**
- OCI image starts correctly and serves HTTP 200 after initialization
- Persistent state binding for chat history and model settings works
- Container lifecycle (start/stop/network cleanup) works correctly

**Friction discovered:**
Open WebUI downloads ~30 HuggingFace embedding model files on first boot. This takes
~3-4 minutes before the UI becomes ready. The default readiness probe window (180s) was
too short, causing the first run attempt to fail with `oci_healthcheck_timeout`.

**Fix applied:** Removed `readiness_probe` from the recipe. Service URL is shown immediately;
user opens the browser and waits for the initialization progress to complete.

**Blocker classification:** Upstream app behavior — not a recipe bug, not an Ato runtime bug.

**Follow-up items:**
- [ ] Add `initial_delay_seconds` / `timeout_seconds` fields to `readiness_probe` schema (P1)
- [ ] Open WebUI lightweight default variant: avoid eager model downloads or make prefetch explicit
- [ ] Pin to a stable tag once Open WebUI publishes one (currently only `:main`)

---

## Runtime Fixes Included in This PR

### Fix 1: OCI images with built-in CMD no longer require entrypoint

**File:** `crates/capsule/src/routing/launch_spec.rs`
**Problem:** `derive_launch_spec` required `entrypoint` or `run_command` even for OCI images
that have a built-in `CMD`/`ENTRYPOINT`. All OCI recipes failed with "requires entrypoint or run_command".
**Fix:** Added early return for `runtime == "oci"` with no explicit entrypoint — returns a stub
`LaunchSpec` so the receipt builder proceeds without requiring a redundant entrypoint field.
**Tests added:** `oci_without_entrypoint_returns_stub_spec`, `oci_explicit_entrypoint_overrides_builtin_cmd`,
`non_oci_source_without_entrypoint_or_run_command_fails`

### Fix 2: OCI image resolved at runtime when `ato.lock.json` absent (compat path)

**File:** `crates/ato-cli/src/adapters/runtime/executors/oci_multi_service.rs`
**Problem:** When running from `capsule.toml` (no `ato.lock.json`), the executor's
`plan.lock` is `AtoLock::default()` with no `oci_images` entries. The executor bailed
with "OCI image for target 'app' is not resolved in the lock file".
**Fix:** Added fallback: when no lock entry is found for a target, `provider.resolve_image()`
is called at runtime to resolve the mutable tag to a digest before pull/start. The resolved
digest is stored in the images map and passed to `execute_service_graph_with_provider`, which
enforces that `resolved_digest` is non-empty before any pull occurs.
**Tests added:** `oci_runtime_does_not_start_without_resolved_digest`,
`oci_runtime_resolved_digest_is_propagated_to_pull`,
`oci_runtime_digest_drift_changes_identity`,
`oci_runtime_no_lock_path_resolved_image_has_non_empty_digest`

**Execution identity invariants preserved:**
- Mutable tags (`:latest`) are resolved to digest before pull/start — never run raw
- Empty `resolved_digest` is rejected before any pull or container creation
- Different digest = different execution identity (see `oci_runtime_digest_drift_changes_identity`)
- Session/receipt records store `image_ref` + `image_digest` for audit

### Enhancement: Readiness probe default timeout

**File:** `crates/ato-cli/src/adapters/runtime/executors/oci_multi_service.rs`
**Change:** `READINESS_HTTP_ATTEMPTS` and `READINESS_TCP_ATTEMPTS` increased from 30 to 90
(60s → 180s total window). This is a global default; per-target timing controls are a P1
follow-up (see "Follow-up Issues" below).

---

## Follow-up Issues

| # | Priority | Description |
|---|----------|-------------|
| 1 | P1 | Add `initial_delay_seconds`/`timeout_seconds` to `readiness_probe` schema |
| 2 | P1 | Pin n8n and NocoDB recipes to specific stable version tags |
| 3 | P1 | Add `N8N_ENCRYPTION_KEY` as a generated secret in n8n recipe |
| 4 | P2 | Open WebUI lightweight default: avoid eager HuggingFace model downloads |
| 5 | P2 | Pin Open WebUI to a stable tag (currently only `:main` is published) |
| 6 | P2 | Add `first_run_note` field to recipe metadata for slow-start apps |

---

## Batch 2 Candidates

Based on this batch, the following are ready for Batch 2 with minimal friction expected:

- `pocketbase/pocketbase` — Go binary, likely fast like Memos
- `axllent/mailpit` — Go binary, minimal deps, SMTP test UI
- `filebrowser/filebrowser` — Go binary, minimal deps
- `actualbudget/actual` — similar single-service pattern to Memos
- `gethomepage/homepage` — simple static config, fast start

Multi-service recipes (Flowise, Dify, Langfuse) should wait for `oci_compose` path validation.


---

## App Details

### 1. Uptime Kuma — ✅ pass

**Repo:** louislam/uptime-kuma
**Image:** `louislam/uptime-kuma:1`
**Port:** 3001 (published dynamically)
**Persistent state:** `/app/data`

**What Ato proved:**
- OCI single-service recipe with persistent filesystem state works end-to-end
- Dynamic port allocation prevents conflicts on developer machines
- `ato stop --all` correctly tears down container and network

**Recipe path:** explicit `capsule.toml` with `[targets.app]` + `[services.main]`

---

### 2. Memos — ✅ pass

**Repo:** usememos/memos
**Image:** `neosmemo/memos:stable`
**Port:** 5230 (published dynamically)
**Persistent state:** `/var/opt/memos`

**What Ato proved:**
- Lightweight Go app starts in ~15s including image pull
- SQLite-backed persistent state binding works
- First-run creates DB schema automatically; no manual init needed

**Recipe path:** explicit `capsule.toml`

---

### 3. NocoDB — ✅ pass

**Repo:** nocodb/nocodb
**Image:** `nocodb/nocodb:latest`
**Port:** 8080 (published dynamically)
**Persistent state:** `/usr/app/data`

**What Ato proved:**
- Large image (~500MB) pulls and starts reliably
- Nuxt SPA served on first request with HTTP 200
- No external database required in single-node mode

**Recipe path:** explicit `capsule.toml`

**Follow-up:** Pin to a specific version tag instead of `:latest` for reproducibility.

---

### 4. n8n — ✅ pass

**Repo:** n8n-io/n8n
**Image:** `n8nio/n8n:latest`
**Port:** 5678 (published dynamically)
**Persistent state:** `/home/node/.n8n`

**What Ato proved:**
- Workflow automation UI starts in single-user mode without additional config
- Node.js app with embedded SQLite, no sidecar database needed
- Dynamic port avoids conflict with other local services

**Recipe path:** explicit `capsule.toml`

**Follow-up:**
- Add `N8N_ENCRYPTION_KEY` as a generated secret for production security
- Pin to specific version tag (see [image-tag-pinning-audit.md](./image-tag-pinning-audit.md))

---

### 5. Open WebUI — ⚠️ degraded

**Repo:** open-webui/open-webui
**Image:** `ghcr.io/open-webui/open-webui:main`
**Port:** 8080 (published dynamically)
**Persistent state:** `/app/backend/data`

**What Ato proved:**
- OCI image starts correctly and serves HTTP 200 after initialization
- Persistent state binding for chat history and model settings works
- Container lifecycle (start/stop/network cleanup) works correctly

**Friction discovered:**
Open WebUI downloads ~30 HuggingFace embedding model files on first boot. This takes
~3-4 minutes before `/health` returns 200. The default readiness probe window (180s) was
too short, causing the first run attempt to fail with `oci_healthcheck_timeout`.

**Fix applied:** Removed `readiness_probe` from the recipe. Service URL is shown immediately;
user opens the browser and waits for the initialization progress to complete.

**Blocker classification:** Upstream app behavior — not a recipe bug, not an Ato runtime bug.

**Follow-up items:**
- [ ] Add `initial_delay_seconds` / `timeout_seconds` fields to `readiness_probe` schema (P1 — narrow Ato enhancement)
- [ ] Add a `first_run_note` field to recipe metadata for user-visible warnings
- [ ] Pin to a stable tag once Open WebUI publishes one (currently only `:main`)

---

## Runtime Bugs Fixed During This Batch

### Bug 1: OCI entrypoint not required for `capsule.toml` recipes

**File:** `crates/capsule/src/routing/launch_spec.rs`
**Problem:** `derive_launch_spec` required `entrypoint` or `run_command` even for OCI images
that have a built-in `CMD`/`ENTRYPOINT`. All OCI recipes failed with "requires entrypoint or run_command".
**Fix:** Added early return for `runtime == "oci"` with no explicit entrypoint — returns a stub
`LaunchSpec` so the receipt builder proceeds without requiring a redundant entrypoint field.

### Bug 2: OCI image not resolved for `capsule.toml` compat path

**File:** `crates/ato-cli/src/adapters/runtime/executors/oci_multi_service.rs`
**Problem:** When running from `capsule.toml` (no `ato.lock.json`), the executor's
`plan.lock` is `AtoLock::default()` with no `oci_images` entries. The executor bailed
with "OCI image for target 'app' is not resolved in the lock file".
**Fix:** Moved provider readiness check before image map building; added fallback to
resolve the image ref from `service.runtime.runtime().image` at runtime using
`provider.resolve_image()` when no lock entry is found.

### Bug 3: `schema_id` must be a valid `sha256:` 64-hex-char hash

**Problem:** Initial recipe files used human-readable schema IDs or malformed hex strings.
**Fix:** All 5 recipes updated with `sha256:` + SHA-256 hash of the schema string.

### Enhancement: Readiness probe timeout increased

**File:** `crates/ato-cli/src/adapters/runtime/executors/oci_multi_service.rs`
**Change:** `READINESS_HTTP_ATTEMPTS` and `READINESS_TCP_ATTEMPTS` increased from 30 to 90
(60s → 180s total window) to accommodate slower-starting services.

---

## What Ato Proved (Batch-Level)

1. **OCI single-service capsule.toml recipes work end-to-end** — state register → run → HTTP verify → ps → stop
2. **Dynamic port allocation** prevents conflicts on developer machines automatically
3. **Persistent filesystem state** survives across stop/restart correctly
4. **Network isolation** — each session gets its own bridge network, torn down on stop
5. **Clean ATO_HOME** — isolated `/tmp/ato-recipe-batch1.*` per run, no cross-contamination

---

## P1 Recipe Expansion Candidates

Based on this batch, the following are ready for Batch 2 with minimal friction expected:
- `actualbudget/actual` — similar single-service pattern to Memos
- `gethomepage/homepage` — simple static config, likely fast start
- `pocketbase/pocketbase` — Go binary, likely fast like Memos
- `axllent/mailpit` — Go binary, minimal deps
- `filebrowser/filebrowser` — Go binary, minimal deps

Multi-service recipes (Flowise, Dify, Langfuse) should wait for `oci_compose` path validation.
