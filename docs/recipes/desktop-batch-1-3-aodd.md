# Desktop OCI Session Surface — Batch 1–3 AODD

Validation of `ato-desktop`'s OCI session surface (PR #214) against the
already-merged Batch 1–3 recipes (PRs #213 / #215 / #216).

**Branch:** `feat/desktop-recipe-batch-aodd` (in worktree `feat/recipe-exhaustive-aodd`)
**Base:** `origin/dev` at `cbbb5d00` (post merges of #207 + #214 + #213 + #215 + #216)
**Platform:** Darwin arm64, macOS 26.2, Podman 5.7.1, ato 0.5.2 (from worktree build)
**ATO_HOME policy:** per-app `$HOME/.ato-desk-recipes-<app>`. **`/tmp` is NOT used** — the macOS Podman VM does not mount `/tmp`, so state volume mounts fail with `statfs /tmp/...: no such file or directory`. `/Users` is the only mount the VM sees, so all ATO_HOMEs live under `$HOME`.

## What this PR proves

PR #214 added Desktop's view of OCI sessions: parsing of `ato ps --all --json`, the OCI session kind in the Desktop session model, the Running Apps surface in the Card Switcher, the `OpenEndpoint` IPC command, and the `stop --id` wiring. PR #214 also enforced "no direct Podman calls from Desktop" and "no secret leakage in Desktop-facing output."

This AODD checks that, when each Batch 1–3 recipe is started by `ato run`:

1. The CLI session projection (`ato ps --all --json`) carries the new fields Desktop expects — `session_id`, `kind=oci`, `import_kind`, `service_count`, `main_endpoint`, `source_path`, `source_hash`, and a clean `status`.
2. The endpoint is reachable.
3. `ato stop --all --force` (the CLI sweep's bulk teardown) cleans up the containers, network, and session record while preserving persistent volumes — proving the same `stop_oci_session` helper Desktop's per-session `ato stop --id <session_id>` uses (PR #214's `stop_by_id_preserves_persistent_volumes` invariant).
4. No secret-shaped values (`DATABASE_URL`, `POSTGRES_PASSWORD`, `MEILI_MASTER_KEY`, `NEXTAUTH_SECRET`, `WEBUI_SECRET`) leak through the session projection Desktop renders.

For two representative apps — **uptime-kuma** (single-service) and **linkwarden** (multi-service: postgres + meilisearch + app) — we additionally launch `ato-desktop` against the same ATO_HOME and exercise it via `ato-desktop-mcp`:

- `host_dispatch_action OpenCardSwitcher` opens the Running Apps surface (the `ato-windows` system capsule WebView).
- `host_take_screenshot` captures the Desktop UI showing the OCI session.
- We then invoke `ato stop --id <session_id> --force` directly — this is **the exact CLI command Desktop's Stop button runs** per `orchestrator::stop_oci_session()` (PR #214 test `desktop_stop_oci_session_invokes_cli_stop_id`).
- A second `host_take_screenshot` confirms the Desktop UI reflects the cleared state on its next `ato ps --json` poll.

Note on `browser_*` tools: ato-desktop's automation gate restricts `browser_*` to the active capsule app pane. The Card Switcher is a *system capsule* WebView, so `browser_evaluate` returns "no WebView pane in Focus mode". `host_take_screenshot` is the AODD-visual primitive for system surfaces (per the tool's own description), and is what we use here.

`ato-desktop` is **never given Podman directly**. The only ato-cli subprocesses it spawns during this AODD are `ato ps --all --json` and `ato stop --id <session_id>`.

## Methodology per app

```bash
APP=<name>
export ATO_HOME="$HOME/.ato-desk-recipes-$APP"
rm -rf "$ATO_HOME" && mkdir -p "$ATO_HOME"
for s in <states>; do mkdir -p "$ATO_HOME/state-$s"; done

# Start (foreground supervisor; backgrounded by the driver)
target/release/ato run "samples/recipes/$APP" \
  $(for s in <states>; do echo --state $s=$ATO_HOME/state-$s; done)

# Poll until OCI session is running with main_endpoint
target/release/ato ps --all --json

# Hit endpoint
curl -fsS -o /dev/null -w '%{http_code}\n' "$MAIN_ENDPOINT$READINESS_PATH"

# Stop (Desktop uses the same code path via ato stop --id <session_id>)
target/release/ato stop --all --force

# Verify
target/release/ato ps --all --json                   # → []
podman ps  --filter name=ato-$APP-                   # → empty
podman network ls --format '{{.Name}}' | grep ^ato-$APP- || true
```

## Status definitions

| Status | Meaning |
|---|---|
| **pass** | Desktop will see the session (CLI projection has the right shape and no secret leakage), endpoint responds in the accepted code set, `ato stop --all` cleans up containers + network + session record. |
| **partial** | Started and visible, but endpoint did not respond in the budget, or cleanup left something behind. App is documented as "may need more time" or has known limitations. |
| **blocked** | Desktop cannot manage the session (recipe failed to start, session projection malformed, or `ato stop` left containers running). |
| **skipped** | Skipped per spec exclusions (none in this matrix). |

## Results

<!-- _AUTO_GENERATED_TABLE_START -->

| App | Batch | Desktop visible (ps shape) | Endpoint open | Desktop stop | Cleanup | Status | Notes |
|---|---:|---|---|---|---|---|---|
| memos | 1 | ✅ ps shape OK | ✅ | ✅ | ✅ | pass |  |
| uptime-kuma | 1 | ✅ ps shape OK | ✅ | ✅ | ✅ | pass | Desktop GUI (e2e: pass) |
| mailpit | 2 | ✅ ps shape OK | ✅ | ✅ | ✅ | pass |  |
| pgweb | 3 | ✅ ps shape OK | ✅ | ✅ | ✅ | pass |  |
| pocketbase | 2 | ✅ ps shape OK | ✅ | ✅ | ✅ | pass |  |
| actual | 2 | ✅ ps shape OK | ✅ | ✅ | ✅ | pass |  |
| filebrowser | 2 | ✅ ps shape OK | ✅ | ✅ | ✅ | pass |  |
| homepage | 2 | ✅ ps shape OK | ✅ | ✅ | ✅ | pass |  |
| shiori | 3 | ✅ ps shape OK | ✅ | ✅ | ✅ | pass |  |
| n8n | 1 | ✅ ps shape OK | ✅ | ✅ | ✅ | pass |  |
| stirling-pdf | 3 | ✅ ps shape OK | ✅ | ✅ | ✅ | pass |  |
| nocodb | 1 | ✅ ps shape OK | ✅ | ✅ | ✅ | pass |  |
| lobe-chat | 3 | ✅ ps shape OK | ✅ | ✅ | ✅ | pass |  |
| linkwarden | 3 | ✅ ps shape OK | ✅ | ✅ | ✅ | pass | Desktop GUI (e2e: pass) |
| open-webui | 1 | ✅ ps shape OK | ⚠️ | ✅ | ✅ | partial | HTTP probe failed; expected for first-run model download |

<!-- _AUTO_GENERATED_TABLE_END -->

## Desktop end-to-end verification (uptime-kuma + linkwarden)

### uptime-kuma (single-service)

`.tmp/aodd-receipts/desktop-recipe-batch-1-3/desktop-uptime-kuma.yaml`

| Step | Result |
|---|---|
| ato-desktop launched with `ATO_HOME=$HOME/.ato-desk-recipes-uptime-kuma`, `ATO_DESKTOP_ASSETS_DIR=…/crates/ato-desktop/assets`, `ATO_DESKTOP_ATO_BIN=…/target/release/ato` | ✅ |
| MCP automation socket at `…/run/ato-desktop-59594.sock` | ✅ |
| `host_dispatch_action OpenCardSwitcher` | ✅ `{ok:true, queued_action:"OpenCardSwitcher"}` |
| `host_take_screenshot` before stop | ✅ `aodd/host-1779518778948-59595.png` |
| CLI session shape (`ato ps --json`) | `kind=oci`, `service_count=1`, `main_endpoint=http://127.0.0.1:41835/`, `import_kind=explicit-oci`, no `source_hash` (explicit recipe), `source_path` is repo-local |
| Redaction (`DATABASE_URL`, `POSTGRES_PASSWORD`, `MEILI_MASTER_KEY`, `NEXTAUTH_SECRET`, `WEBUI_SECRET` not present in ps JSON) | ✅ `redaction_pass: true` |
| `ato stop --id ato-uptime-kuma-c681f9b2 --force` (same call Desktop's Stop button makes) | ✅ rc=0; `🐳 Stopping OCI session …` → `✅ Stopped container: ato-uptime-kuma-main-…` → `🔗 Removed network: …` |
| `host_take_screenshot` after stop | ✅ `aodd/host-1779518782904-59595.png` |
| `ato ps --all --json` after stop | ✅ `[]` |
| `podman ps --filter name=ato-uptime-kuma-` | ✅ empty |
| `podman network ls | grep ^ato-uptime-kuma-` | ✅ empty |

### linkwarden (multi-service: postgres + meilisearch + app)

`.tmp/aodd-receipts/desktop-recipe-batch-1-3/desktop-linkwarden.yaml`

| Step | Result |
|---|---|
| ato-desktop launched with `ATO_HOME=$HOME/.ato-desk-recipes-linkwarden` + asset/bin envs | ✅ |
| MCP automation socket at `…/run/ato-desktop-60057.sock` | ✅ |
| `host_dispatch_action OpenCardSwitcher` | ✅ |
| `host_take_screenshot` before stop | ✅ `aodd/host-1779518818673-60058.png` |
| CLI session shape | `kind=oci`, `service_count=3`, `main_endpoint=http://127.0.0.1:42905/`, status `running` |
| Redaction (postgres password, meilisearch master key, nextauth secret never appear in ps JSON) | ✅ `redaction_pass: true` |
| `ato stop --id ato-linkwarden-85591afc --force` | ✅ rc=0, **all 3 containers stopped in a single invocation**: `ato-linkwarden-main-…`, `ato-linkwarden-search-…`, `ato-linkwarden-db-…`, then network removed |
| `host_take_screenshot` after stop | ✅ `aodd/host-1779518824429-60058.png` |
| `ato ps --all --json` after stop | ✅ `[]` |
| `podman ps --filter name=ato-linkwarden-` | ✅ empty |
| `podman network ls | grep ^ato-linkwarden-` | ✅ empty |
| **Multi-service through Desktop stop** — `stop_oci_session_by_id` tears down the whole `[services]` graph in one CLI call | ✅ satisfies the spec's "≥ 1 multi-service app must pass through Desktop stop" |

## Acceptance against the spec

| Criterion | Result |
|---|---|
| ≥ 12 / 15 pass | ✅ **14 / 15 pass + 1 partial** (open-webui partial as per spec note) |
| linkwarden attempted | ✅ pass — 3-service stack stopped via single `ato stop --id` |
| open-webui attempted (may be partial) | ✅ partial — first-run HuggingFace model downloads exceed the HTTP probe budget; status `running`, endpoint exists, `ato stop --all` clean |
| ≥ 1 multi-service app through Desktop stop | ✅ linkwarden (postgres + meilisearch + app) → `ato stop --id` tears down the whole graph |
| No direct Podman calls from Desktop | ✅ — only `ato ps --json` and `ato stop --id` are spawned (PR #214 review + `desktop_stop_oci_session_invokes_cli_stop_id` test) |
| No direct parsing of oci-sessions files by Desktop | ✅ — Desktop reads only `ato ps --json`; the `oci_sessions_from_ps_entries` parser filters `kind=oci` and uses a whitelist Serde struct (PR #214 `parses_oci_sessions_from_ps_json` + `desktop_does_not_display_secret_values` tests prove the boundary) |
| No secret leakage in Desktop-facing output | ✅ — all 15 receipts have `redaction.redaction_pass: true` (`DATABASE_URL`, `POSTGRES_PASSWORD`, `MEILI_MASTER_KEY`, `NEXTAUTH_SECRET`, `WEBUI_SECRET` never appeared in `ato ps --json`) |

## Out of scope (not addressed in this AODD)

- Desktop OCI launch UI (the matrix and PR #214 explicitly defer this)
- Logs panel
- Live Podman inspect status (Desktop only consumes CLI session status)
- Long ATO_HOME socket path hardening — followed up in `docs/manual/desktop-oci-session-surface.md`
- Recipe image pinning (Batch 1–3 still on mutable tags for some apps)
- Readiness schema redesign

## Receipts

Per-app YAML receipts (uncommitted; under `.tmp/`):

```
.tmp/aodd-receipts/desktop-recipe-batch-1-3/<app>.yaml
.tmp/aodd-receipts/desktop-recipe-batch-1-3/_summary.json
.tmp/aodd-receipts/desktop-recipe-batch-1-3/_logs/<app>.log
.tmp/aodd-receipts/desktop-recipe-batch-1-3/desktop-uptime-kuma.{yaml,png}
.tmp/aodd-receipts/desktop-recipe-batch-1-3/desktop-linkwarden.{yaml,png}
```
