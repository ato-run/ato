# Recipe Batch 2 — AODD Results

**Branch:** `feat/initial-recipe-batch-2` (commit `1558b9b5`)  
**Validated by:** `ato` v0.5.2 (local dev build, `target/debug/ato`)  
**ATO_HOME policy:** fresh `$HOME/.ato-batch2/<app>/` per run (not `/tmp` — see note below)  
**Platform:** macOS arm64, Podman machine-based OCI  
**Date:** 2026-05-23

## Summary

> **Note:** For consolidated catalog status, see [initial-catalog-status.md](./initial-catalog-status.md).
> For image tag pinning audit, see [image-tag-pinning-audit.md](./image-tag-pinning-audit.md).
> For secret cleanup plan, see [secret-cleanup-plan.md](./secret-cleanup-plan.md).

All 5 Batch 2 targets passed clean ATO_HOME AODD on first attempt.

| App | Status | Recipe path | Startup | Endpoint | State | Cleanup | Notes |
|---|---|---|---:|---|---|---|---|
| mailpit | ✅ pass | `samples/recipes/mailpit/` | 9s | HTTP 200 `/` | `data:/data` | clean | zero-config SMTP capture UI |
| pocketbase | ✅ pass | `samples/recipes/pocketbase/` | 6s | HTTP 200 `/_/` | `data:/pb_data` | clean | admin UI at `/_/` not `/` |
| filebrowser | ✅ pass | `samples/recipes/filebrowser/` | 9s | HTTP 200 `/` | `data:/database`, `files:/srv` | clean | `FB_PORT=8080` env var required — see below |
| actual | ✅ pass | `samples/recipes/actual/` | 9s | HTTP 200 `/` | `data:/data` | clean | local-first budget app |
| homepage | ✅ pass | `samples/recipes/homepage/` | 6s | HTTP 200 `/` | `config:/app/config` | clean | personal dashboard |

## App Details

### mailpit (`axllent/mailpit:latest`)
- **Image digest (AODD):** `sha256:60d1dbefeabf...`
- **Recipe path:** explicit `capsule.toml`, single OCI service
- **What Ato proved:** zero-config SMTP web UI runs with a single state binding
- **Notes:** no auth required for demo; suitable for local dev email testing

### pocketbase (`ghcr.io/muchobien/pocketbase:latest`)
- **Image digest (AODD):** `sha256:2d41181ceeca...`
- **Recipe path:** explicit `capsule.toml`, single OCI service
- **What Ato proved:** Go-based lightweight backend with embedded admin UI
- **Notes:** readiness probe must target `/_/` (not `/` which returns 404 before setup)

### filebrowser (`filebrowser/filebrowser:latest`)
- **Image digest (AODD):** `sha256:9ffebe23dc98...`
- **Recipe path:** explicit `capsule.toml`, single OCI service
- **What Ato proved:** multi-state recipe (database + files) works correctly
- **Runtime discovery:** default container image runs filebrowser on port 80,
  which fails with `permission denied` when the container runs as non-root.
  Fixed by setting `FB_PORT=8080` and `FB_ADDRESS=0.0.0.0` env vars.
- **Follow-up:** pin to a stable version tag; document default admin credentials
  printed to logs on first run

### actual (`actualbudget/actual-server:latest`)
- **Image digest (AODD):** `sha256:7228365ca65e...`
- **Recipe path:** explicit `capsule.toml`, single OCI service
- **What Ato proved:** local-first finance app with persistent data state
- **Notes:** first-run shows onboarding wizard; no auth bypass needed

### homepage (`ghcr.io/gethomepage/homepage:latest`)
- **Image digest (AODD):** `sha256:8e5f595273f0...`
- **Recipe path:** explicit `capsule.toml`, single OCI service
- **What Ato proved:** config-driven dashboard; config state pre-populated by container on first run
- **Notes:** startup ~6s; config at `/app/config` is populated automatically on first run

## Runtime Discoveries

### `/tmp` not mounted in Podman machine VM (macOS)
On macOS, Podman runs in a Linux VM. Only paths under `$HOME` (and certain Apple-managed paths)
are shared into the VM by default. State paths under `/tmp` cause:
```
statfs /tmp/...: no such file or directory
```
**Workaround:** Use `$HOME/...` for both `ATO_HOME` and `--state` paths in AODD.  
**Follow-up:** Document this in Ato CLI help / error message; consider detecting and surfacing
the error with a clear diagnostic rather than the generic container creation failure.

### filebrowser port 80 permission denied in rootless containers
The `filebrowser/filebrowser` image defaults to port 80, which requires root.
Rootless podman containers (the default on macOS/Linux) cannot bind port 80 inside the container.  
**Fix applied:** `FB_PORT=8080` + `FB_ADDRESS=0.0.0.0` env vars in recipe.

## Follow-up Issues

1. **Pin recipe image tags** — all recipes use `:latest`; pin to a stable digest or version tag
   (pocketbase, mailpit, filebrowser, actual, homepage)
2. **Podman `/tmp` mount diagnostic** — surface a clear error when state path is not accessible
   inside the podman VM (currently shows generic container creation failure)
3. **filebrowser admin credentials UX** — first-run credentials are printed to container logs only;
   consider surfacing them in `ato run` output
4. **Batch 3 candidates:** Stirling-PDF, linkwarden, nocodb-revisit (pin), lobe-chat, pgweb

## Validation

```
cargo fmt --all            — pass
cargo check -p capsule -p ato-cli  — pass
cargo test -p ato-cli oci_multi_service --lib  — pass
cargo test -p ato-cli oci_session --lib  — pass
cargo test -p capsule oci_compose_lock --lib  — pass
git diff --check  — pass
```
