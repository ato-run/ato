# Desktop sample-recipes AODD — post-fix verification (Phase 1 ✓, Phase 2 routing ✓, Phase 2 launch ✗ on state binding)

**Branch:** `test/desktop-sample-recipes-aodd-verified` (this PR)
**Base:** `dev` @ `1758a981` + 5-file local fix (preflight routing + Desktop visible-error + Blinko/AFFiNE recipes + catalog registration)
**Supersedes:** PR #254 (pre-fix baseline)
**Companion PR:** #253 (preflight routing finding — this AODD verifies the suggested fix resolves it)
**Date:** 2026-05-24

## Headline

The routing slice is **complete and verified end-to-end**. CLI Phase 1 passes 24/24. Desktop
Phase 2 reaches consent → approve → launch start for all 4 apps driven (Memos as Test Set A
spot-check + Blinko / AFFiNE / Dify as new targets). Desktop Phase 3 negative test confirms
silent stall is gone — `WARN consent preflight failed — wizard shows error state` fires
instead of `continuing with launch fallback`.

The remaining `session-created` gap is at the next layer down: `ato app session start`
rejects all 4 driven apps with `state '<X>' requires an explicit persistent binding before
it can be attached`. This is a downstream session-binding issue, NOT routing.

## Phase 1 — CLI smoke (24/24 PASS)

Fresh `ATO_HOME=$(mktemp -d -t ato-rerun-aodd-XXXXXX)`. Same 8-target matrix as PR #254.

| Target | `app resolve <alias>` | `internal preflight <alias>` | `internal preflight <github>` |
|---|---|---|---|
| memos | ✅ kind=sample_recipe | ✅ exit 0, capsule_id=memos v0.24.0 | ✅ exit 0, capsule_id=memos v0.24.0 |
| uptime-kuma | ✅ kind=sample_recipe | ✅ exit 0, capsule_id=uptime-kuma v1.23.16 | ✅ exit 0 |
| n8n | ✅ kind=sample_recipe | ✅ exit 0, capsule_id=n8n v1.0.0 | ✅ exit 0 |
| open-webui | ✅ kind=sample_recipe | ✅ exit 0, capsule_id=open-webui v0.6.13 | ✅ exit 0 |
| excalidraw | ✅ kind=sample_recipe | ✅ exit 0, capsule_id=excalidraw v0.17.6 | ✅ exit 0 |
| **blinko** | ✅ kind=sample_recipe | ✅ exit 0, capsule_id=blinko v0.1.0 | ✅ exit 0 |
| **affine** | ✅ kind=sample_recipe | ✅ exit 0, capsule_id=affine v0.26.0 | ✅ exit 0 |
| **dify** | ✅ kind=sample_recipe | ✅ exit 0, capsule_id=dify v1.14.2 | ✅ exit 0 |

Compare with PR #254 (pre-fix): every preflight cell was `E999 unsupported preflight target`
or `E999 manifest path does not exist`. The routing fix moves all 16 cells from FAIL to
PASS, plus the 3 new catalog entries (Blinko / AFFiNE / Dify) lift their `app resolve` from
FAIL to PASS.

## Phase 2 — Desktop drive (routing PASS, launch FAIL on state binding)

Hermetic ato-desktop 0.5.2, `--skip-onboarding`, Focus mode, MCP automation. For each app:
NavigateToUrl → wait → `host_dispatch_action[ForceApprovePending]` → observe.

| App | NavigateToUrl | Preflight | Materialized recipe | ForceApprovePending consumed pending | session start | session-created |
|---|---|---|---|---|---|---|
| memos | ✅ queued | ✅ (no error log, no continuing-fallback warn) | `/tmp/.../sample-recipes/memos/capsule.toml` | ✅ "consuming pending target" | ❌ state 'data' explicit binding | ❌ |
| blinko | ✅ queued | ✅ | `/tmp/.../sample-recipes/blinko/capsule.toml` | ✅ | ❌ state 'db-data' explicit binding | ❌ |
| affine | ✅ queued | ✅ | `/tmp/.../sample-recipes/affine/capsule.toml` | ✅ | ❌ state 'db-data' explicit binding | ❌ |
| dify | ✅ queued | ✅ | `/tmp/.../sample-recipes/dify/capsule.toml` | ✅ | ❌ state 'api-storage' explicit binding | ❌ |

Pattern: routing works end-to-end for all four. Sample recipe is materialized to hermetic
`$ATO_HOME/sample-recipes/<slug>/capsule.toml`. Consent wizard hydrates (no preflight error,
no silent stall). `ForceApprovePending` finds a real pending consent target — implying the
wizard genuinely showed Approve as a live action — and consumes it. The launch flow then
calls `ato app session start <handle>`, which rejects every app for the same reason: every
sample recipe declares one or more `[state.<X>]` blocks with `attach = "explicit"`, and the
Desktop launch flow does not provide a binding before calling session start.

This is the same underlying failure that PR #253 documented for Memos `state 'data'`. The
recipe authoring style (Postgres data dir, app upload dir, etc.) all use `attach = "explicit"`
to ensure operators explicitly opt into persistence — but the Desktop launch UX never
prompts for or synthesizes that binding for sample-recipe targets.

### Visible-error behavior (Approve gating)

`approve_execution_plan_consent` MCP tool still errors with `automation command
Discriminant(22) is not supported in Focus mode (no WebView pane)` — that's the pre-existing
B4 gap from PR #249, not a regression. `host_dispatch_action[ForceApprovePending]` is
documented as the workaround and was used here.

Important note about Approve gating: I cannot directly screenshot to verify the Approve
button is visually disabled when `preflight_failed=true`. The launch_window.rs diff includes
a new test (`remote_manifest_missing_preflight_blocks_approve`) that asserts the data flag,
and `host_dispatch_action[ForceApprovePending]` is a host-level dispatch that bypasses any
UI gating regardless — so its success in the negative test (below) does not prove the UI
button is enabled. The behavior is consistent with "UI shows error state, Approve disabled;
ForceApprovePending is an automation bypass."

## Phase 3 — Negative test (PASS — silent stall removed)

Handle: `capsule://github.com/ato-run/does-not-exist-sample-recipe`

```text
INFO  Focus-mode NavigateToUrl url=capsule://github.com/ato-run/does-not-exist-sample-recipe
DEBUG calling ato internal preflight handle="capsule://github.com/ato-run/does-not-exist-sample-recipe"
WARN  consent preflight failed — wizard shows error state
      handle="github.com/ato-run/does-not-exist-sample-recipe"
      error=ato internal preflight failed (exit status 2):
        preflight collection failed: manifest path does not exist:
        /tmp/.../external-capsules/github/ato-run/does-not-exist-sample-recipe/capsule.toml
```

Brief criteria:
- ✅ `preflight_failed=true` set (per launch_window.rs diff + this WARN log line)
- ✅ user-visible error in wizard (`shows error state` log; UI not screenshot-verified)
- ✅ NO `continuing with launch fallback` log line — silent fallback path is removed
- ⚠️ Approve disabled — inferred from preflight_failed=true + new test; not screenshot-verified

When `ForceApprovePending` was used to bypass (testing what a real user would see if they
somehow clicked Approve), the launch surfaced the upstream cause at the boot-failure step:

```text
WARN  preflight collection skipped; falling back to lazy aggregation     # second fallback path
ERROR ato_launch: capsule boot failed
      error=...E999: Failed to fetch GitHub install draft (status=404 Not Found):
      {"error":"repo_not_found","message":"The requested GitHub repository could not be found."}
```

So the actionable cause IS reachable — just at the boot step, not at the preflight step.
The user's known follow-up ("upstream 404 などの原因伝播改善") would surface
`repo_not_found` directly from the preflight error, eliminating the two-step delay.

## Phase 4 — Session verification (N/A, nothing reached creation)

```text
$ ato ps --json
[]
$ podman ps
(no ato containers)
$ find $ATO_HOME -name 'session*' -o -name 'receipt*' -o -name 'launch*'
(empty)
$ ls $ATO_HOME/sample-recipes/
affine  blinko  dify
```

Only the materialized sample-recipe manifests exist — the rest of the runtime state never
got created. This is consistent with all 4 apps failing at session-start state binding.

## Final report (per brief format)

```text
AODD complete.

Headline:
  Desktop sample recipe routing: PASS (routing fix verified end-to-end)
  Desktop session-created reach: FAIL (blocked at next layer: state.<X> attach="explicit"
                                       not auto-bound by Desktop launch flow)

Reach rate:
  Existing Test Set A: 0/5 session-created
    (only memos spot-checked via Desktop drive — same state-binding failure as Blinko;
     CLI Phase 1 confirms routing works for all 5)
  New apps:
    Blinko: visible-error  (routing ✓, session start ✗ on state 'db-data') — must-pass NOT MET
    AFFiNE: visible-error  (routing ✓, session start ✗ on state 'db-data')
    Dify:   visible-error  (routing ✓, session start ✗ on state 'api-storage')

Key findings:
  - Preflight routing fix (PR #253's suggested change) is verified end-to-end via Desktop
    NavigateToUrl. The 5-file local diff resolves Class A (preflight gap), Class B (missing
    Blinko/AFFiNE recipes), and Class C (Dify catalog miss) from PR #254 in one go.
  - Silent fallback path removed. `WARN consent preflight failed — wizard shows error
    state` replaces the previous `continuing with launch fallback` warning. No silent
    stall observed in the negative test.
  - NEW BLOCKER class surfaced by post-routing AODD: every sample recipe (memos, blinko,
    affine, dify confirmed; the other 4 likely too based on PR #253's CLI-only evidence)
    declares state blocks with attach="explicit", and `ato app session start` rejects the
    launch without a binding. Desktop launch flow doesn't synthesize one. This is a
    downstream session-binding layer, not routing.
  - Negative-test upstream cause IS surfaced — but at the boot-failure step, not at the
    preflight-error step. The user's known follow-up (propagate `repo_not_found` into
    preflight) would close that delay.

Regression check:
  - internal preflight sample recipe alias: pass (8/8 targets)
  - capsule://github sample recipe mapping: pass (8/8 targets)
  - local path precedence: not_tested (intentional — diff preserves existing precedence;
                                       resolve.rs test `local_path_not_hijacked_by_sample_recipe`
                                       and the new preflight tests cover this)
  - silent fallback removed: pass (no `continuing with launch fallback` line in any path)

Receipts:
  - .tmp/aodd-receipts/desktop-sample-recipes/blinko.yaml
  - .tmp/aodd-receipts/desktop-sample-recipes/affine.yaml
  - .tmp/aodd-receipts/desktop-sample-recipes/dify.yaml
  - .tmp/aodd-receipts/desktop-sample-recipes/negative-missing-sample.yaml

Consolidated doc:
  - docs/recipes/desktop-sample-recipes-aodd.md (this file)

Next slice:
  1. Pick a strategy for state binding when the Desktop launch flow encounters
     `attach="explicit"` (auto-synthesize $ATO_HOME/state/<capsule_id>/<state_name>/,
     OR make session start synthesize it for sample-recipe-sourced launches, OR change
     sample recipes to use a non-explicit attach mode).
  2. Re-run this AODD with that change. Expected outcome:
       - Blinko, Memos: session-created (single-service recipes — should reach HTTP 200)
       - Uptime-Kuma, n8n, open-webui, excalidraw: session-created
       - AFFiNE: session-created OR visible recipe-runtime error (migration container,
         redis dep, etc.)
       - Dify: visible recipe-runtime error (multi-service, arm64 emulation needed)
  3. Land the upstream-cause propagation follow-up in `internal preflight` so the
     negative-test failure reads `repo_not_found` at the preflight step.
```

## Environment

```text
Worktree:  .worktrees/desktop-sample-recipes-aodd-verified   (this PR's branch)
Binaries:  /Users/.../ato target/release/{ato 0.5.2, nacelle 0.5.2}
           /Users/.../crates/ato-desktop/target/release/ato-desktop 0.5.2
           (built from local working tree with the 5-file uncommitted fix)
ATO_HOME:  /tmp/ato-desktop-rerun-aodd                       # hermetic, removed at session end
Desktop:   PID 23818, Focus mode, automation socket at /tmp/.../run/ato-desktop-23818.sock
podman:    applehv machine running; 5 OCI images pre-pulled
  (memos, uptime-kuma, n8n, open-webui, blinkospace/blinko, postgres:14, pgvector/pgvector:pg16,
   redis:7-alpine, ghcr.io/toeverything/affine:stable; excalidraw:0.17.6 unavailable — recipe-runtime issue)
```
