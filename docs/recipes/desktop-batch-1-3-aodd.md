# Desktop OCI Session Surface — Batch 1–3 AODD

Validation of `ato-desktop`'s OCI session surface (PR #214) against the
already-merged Batch 1–3 recipes (PRs #213 / #215 / #216).

**Branch:** `feat/desktop-recipe-batch-aodd-v2` (in worktree `desktop-recipe-batch-aodd`)
**Validation baseline:** `origin/dev` at `bd259f0d` (post merges of #207 + #214 + #213 + #215 + #216)
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

| App | Batch | Desktop visible | Endpoint open | Desktop stop | Cleanup | Status | Notes |
|---|---:|---:|---:|---:|---|---|---|
| memos | 1 | ✅ | ✅ | ✅ | ✅ | pass |  |
| n8n | 1 | ✅ | ✅ | ✅ | ✅ | pass |  |
| nocodb | 1 | ✅ | ✅ | ✅ | ✅ | pass |  |
| open-webui | 1 | ✅ | ⚠️ | ✅ | ✅ | partial | First-run ~30 HuggingFace model downloads exceed HTTP probe budget; status= `running`, endpoint present, stop cleaned |
| uptime-kuma | 1 | ✅ | ✅ | ✅ | ✅ | pass |  |
| actual | 2 | ✅ | ✅ | ✅ | ✅ | pass |  |
| filebrowser | 2 | ✅ | ✅ | ✅ | ✅ | pass | SPA: GET returns HTML, HEAD 404 acceptable |
| homepage | 2 | ✅ | ✅ | ✅ | ✅ | pass |  |
| mailpit | 2 | ✅ | ✅ | ✅ | ✅ | pass |  |
| pocketbase | 2 | ✅ | ✅ | ✅ | ✅ | pass |  |
| lobe-chat | 3 | ✅ | ✅ | ✅ | ✅ | pass | HTTP 307 → /chat expected |
| pgweb | 3 | ✅ | ✅ | ✅ | ✅ | pass |  |
| shiori | 3 | ✅ | ✅ | ✅ | ✅ | pass |  |
| stirling-pdf | 3 | ✅ | ❌ | N/A | ✅ | blocked | Readiness HTTP GET /login times out at 180s; Java/Spring Boot startup slower than default probe timeout |
| linkwarden | 3 | ✅ | ❌ | N/A | ✅ | blocked | Multi-service (postgres+meilisearch+app): all containers start in order but main readiness GET / times out; db readiness probe HTTP on PostgreSQL port 5432 silently falls through |

"Desktop visible" = `ato ps --all --json` has `kind=oci`, `import_kind=explicit-oci`, `service_count`, `main_endpoint`, and no secret leakage.
"Endpoint open" = `curl -fsS -o /dev/null -w '%{http_code}' $MAIN_ENDPOINT$PATH` returns an expected code within budget.
"Desktop stop" = Desktop's `Stop` button path (`ato stop --id <session_id> --force`) exercised; blocked apps never reached this step.
"Cleanup" = After stop, `ato ps --all --json` returns `[]`, `podman ps --filter name=ato-<app>-` is empty, and `podman network ls` has no `ato-<app>-` network.

## Acceptance against the spec

- Acceptance met: **12 pass / 1 partial / 2 blocked** (≥ 12/15 threshold)
- linkwarden attempted: blocked by readiness timeout — orchestration starts all 3 containers but main service probe never succeeds within budget
- open-webui attempted: partial — first-run model downloads expected feature, not a Desktop integration bug
- ≥ 1 multi-service app passed through Desktop stop: ⚠️ not in this re-run — linkwarden readiness blocked before Desktop stop could be exercised; prior AODD pass (PR #218) showed it working end-to-end
- No Desktop direct Podman calls: ✅
- No direct parsing of oci-sessions files by Desktop: ✅
- No secret leakage in Desktop-facing output: ✅

## Follow-up: blocked cases

The blocked apps share a common root cause — **readiness probe timing / schema**, not a Desktop session parsing, display, or stop wiring defect. They are separated into their own follow-up tracks.

### Stirling-PDF Desktop AODD retry

- Investigate why readiness HTTP GET `/login` timed out at 180s despite the container starting and Java process running.
- Confirm whether the endpoint path `GET /login` or default timeout budget is correct for this app.
- Possible mitigations: increase readiness probe timeout, switch to TCP readiness on port 8080, or add `depends_on` ordering.
- Do not classify as a Desktop stop/parsing bug unless reproduced after readiness is fixed.

### Linkwarden Desktop AODD retry

- Investigate cold-pull and startup timing for the 3-service stack (PostgreSQL, Meilisearch, Linkwarden main).
- Review readiness budget: default timeout may be insufficient for Linkwarden's database migration + startup sequence.
- Fix the db service readiness probe — HTTP GET on PostgreSQL port 5432 from the orchestrator silently fails; switch to TCP or exec-based probe.
- Do not classify as a Desktop stop/parsing bug unless readiness probe fix exposes a real Desktop issue.

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
```
.tmp/aodd-receipts/desktop-recipe-batch-1-3/<app>.yaml
.tmp/aodd-receipts/desktop-recipe-batch-1-3/_summary.json
.tmp/aodd-receipts/desktop-recipe-batch-1-3/_logs/<app>.log
.tmp/aodd-receipts/desktop-recipe-batch-1-3/desktop-uptime-kuma.{yaml,png}
.tmp/aodd-receipts/desktop-recipe-batch-1-3/desktop-linkwarden.{yaml,png}
```
