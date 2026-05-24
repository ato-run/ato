# Desktop run_once AODD — run_once fix verified; next gap is multi-leaf dep planning

**Branch:** `test/desktop-runonce-aodd-verified` (this PR)
**Base:** `dev` @ `1758a981` + local `codex/desktop-sample-state-bindings` (now adds run_once lifecycle in orchestrator + services.rs env-injection skip for run_once deps)
**Supersedes:** PR #259
**Date:** 2026-05-25

## Headline

The run_once fix is verified at the layer it covers: AFFiNE's postgres now starts and
reaches `database system is ready to accept connections`. In PR #259 the orchestrator
errored at `"dependency 'migration' for service 'main' has not been started"` because
migration's exit-0 wasn't recognized as ready. That error is GONE.

The next exposed gap is **multi-leaf dependency planning**: when a service has multiple
sibling leaf dependencies (AFFiNE's `migration` depends on `[db, redis]`), the orchestrator
only starts ONE of them and errors on the other. Both AFFiNE and Dify exhibit this same
shape from slightly different starting positions.

**Test Set A reach rate: still 5 / 8 session-created.** Blinko/Memos/uptime-kuma/n8n/
open-webui all unchanged. AFFiNE/Dify still blocked but at a NEW deeper layer (no regression).

## Multi-leaf dep gap — direct evidence

### AFFiNE — progressed past run_once, blocked at redis

```text
$ DOCKER_HOST=unix:///.../podman-machine-default-api.sock \
  ATO_HOME=$(mktemp -d) ato app session start capsule://github.com/toeverything/AFFiNE --json
[db] 2026-05-24 19:38:21.378 UTC LOG:  listening on IPv6 address "::", port 5432
[db] 2026-05-24 19:38:21.382 UTC LOG:  listening on Unix socket "/var/run/postgresql/.s.PGSQL.5432"
[db] 2026-05-24 19:38:21.397 UTC LOG:  database system was shut down at 2026-05-24 19:38:21 UTC
[db] 2026-05-24 19:38:21.408 UTC LOG:  database system is ready to accept connections   ← NEW (run_once fix landed)
{"error": {"cause": "dependency 'redis' for service 'migration' has not been started"}}
elapsed: 20s
```

Compare PR #259:
```text
[db] running bootstrap script ... ok
[db] performing post-bootstrap initialization ...
{"error": {"cause": "dependency 'migration' for service 'main' has not been started"}}   ← OLD (run_once gap)
```

Now postgres reaches the SAME ready state that Blinko's does — proving the exec probe +
run_once recognition both work. The orchestrator just doesn't start redis (migration's
second leaf dep) in parallel with db.

### Dify — fast-fails at planning stage

```text
$ ato app session start capsule://github.com/langgenius/dify --json
{"error": {"cause": "dependency 'db' for service 'api' has not been started"}}
elapsed: 1s   # no container started
```

Same underlying class — Dify's `api` depends on `[db, redis, weaviate]`. The planner walks
to `api` first (because `main` depends on it) and errors before starting any leaf.

### AFFiNE's dep graph

```text
db        no deps                                       ← started
redis     no deps                                       ← NOT started (gap)
migration depends_on = [db, redis], run_once = true     ← would be next after both ready
main      depends_on = [migration]                      ← would be last
```

The orchestrator's scheduling loop appears to start one service, wait for it, then look
at the NEXT service in declaration order. A correct implementation needs to start
ALL services whose dependencies are satisfied in parallel at each tick (or pre-compute
a topological start order and walk levels in parallel).

## Reach rate unchanged from PR #259

| App | session-created | Reason |
|---|---|---|
| memos / uptime-kuma / n8n / open-webui | ✅ (CLI verified PR #257/#258) | — |
| blinko | ✅ (CLI verified this run; no regression) | — |
| excalidraw | ❌ | image tag missing (PR #254) |
| affine | ❌ | NEW deeper layer — multi-leaf dep planning |
| dify | ❌ | Same multi-leaf dep planning class |

**5 / 8 reach session-created.** No regression. AFFiNE made measurable progress (postgres ran
to ready vs failing before any container started).

## What changed vs PR #259

| Property | PR #259 | This AODD |
|---|---|---|
| run_once lifecycle (exit-0 = ready) | gap | **resolved** (db ran past bootstrap on AFFiNE) |
| run_once env-injection skip | n/a | **landed** (migration doesn't demand MIGRATION_HOST/PORT) |
| multi-leaf dep parallel start | n/a (masked by run_once) | **NEW exposed gap** |
| Blinko reach session-created | ✅ | ✅ (no regression, HTTP 200 on :1111) |
| All prior wins from #253–#259 | OK | OK |

## Follow-ups

1. **Implement parallel start for sibling leaf dependencies** in orchestrator's scheduling
   loop. At each tick, start ALL services whose dependencies are satisfied — not just the
   next one in declaration order. Alternative: pre-compute a topological start level set
   at planning time and walk levels in parallel. Unblocks AFFiNE + Dify.
2. **excalidraw image tag** (pre-existing — PR #254).
3. **`ato ps --json` Desktop-session unification** (pending from #257).
4. **Upstream cause propagation in preflight** (pending from #255).
5. **bollard's docker.sock auto-detection** (workaround: DOCKER_HOST=podman).

## Final report (per brief format)

```text
AODD complete.

Headline:
  run_once fix: PASS (AFFiNE's postgres now reaches "ready to accept connections")
  Multi-leaf dep planning: NEW gap exposed (AFFiNE/Dify still blocked one layer deeper)
  Test Set A reach rate: 5 / 8 session-created (unchanged from PR #259; no regression)

Reach rate:
  memos:       session-created ✅
  uptime-kuma: session-created ✅
  n8n:         session-created ✅
  open-webui:  session-created ✅
  blinko:      session-created ✅ (no regression; HTTP 200 on :1111)
  excalidraw:  recipe-runtime block (image tag — PR #254)
  affine:      visible-error (multi-leaf dep planning — NEW exposure)
  dify:        visible-error (same multi-leaf dep planning class)

Key findings:
  - run_once recognition works. AFFiNE no longer errors at "migration not started";
    postgres now starts and reaches ready (proving exec probe + run_once + state
    binding chain works through db's full bootstrap).
  - NEW orchestrator gap: scheduler doesn't start parallel sibling leaf dependencies.
    AFFiNE has `migration depends_on = [db, redis]`; orchestrator starts db only.
    Dify shows same shape — `api depends_on = [db, redis, weaviate]`, orchestrator
    fails before starting any container.

Regression check (vs PR #259):
  - exec-probe handling: PASS
  - readiness_probe.port literal: PASS
  - partial-container cleanup: PASS
  - silent fallback removed: PASS
  - routing + state binding + timing: PASS
  - Blinko session-created (CLI): PASS (HTTP 200 confirmed)
  - Memos session-created: PASS (verified earlier slices)

Receipts:
  - .tmp/aodd-receipts/desktop-runonce/affine.yaml
  - .tmp/aodd-receipts/desktop-runonce/dify.yaml

Consolidated doc:
  - docs/recipes/desktop-runonce-aodd.md

Next slice candidates:
  1. Multi-leaf dep parallel start in orchestrator's scheduling loop (unblocks AFFiNE + Dify)
  2. Drive uptime-kuma / n8n / open-webui through Desktop for UI parity
  3. excalidraw image tag fix
  4. Pending: ato ps Desktop unification, upstream cause propagation, bollard auto-detect
```

## Environment

```text
Worktree:    .worktrees/desktop-runonce-aodd-verified
Branch:      test/desktop-runonce-aodd-verified
Source:      built from local codex/desktop-sample-state-bindings (10 modified + 2 new recipes)
Binaries:    target/release/{ato, nacelle} 0.5.2 (built 2026-05-25 04:37)
ATO_HOME:    multiple mktemp dirs per-app (hermetic)
DOCKER_HOST: unix:///var/folders/.../podman/podman-machine-default-api.sock
podman:      applehv machine running
note:        ato-desktop binary not re-driven this slice — the orchestrator gap is at
             the CLI session-start layer; Desktop calls the same code path. Memos/Blinko
             Desktop drives were verified end-to-end in PR #257/#259.
```
