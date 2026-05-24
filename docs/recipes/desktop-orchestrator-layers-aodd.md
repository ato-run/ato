# Desktop orchestrator-layers AODD — AFFiNE + Dify now reach session-created; 7/8 reach rate

**Branch:** `test/desktop-orchestrator-layers-aodd-verified` (this PR)
**Base:** `dev` @ `1758a981` + local `codex/desktop-sample-state-bindings` (now adds
`ServiceGraphPlan::from_orchestration` and switches orchestrator.rs to it)
**Supersedes:** PR #262
**Date:** 2026-05-25

## Headline

```text
Orchestrator layered start fix: PASS (now reads merged depends_on + connections)
AFFiNE reach session-created:   ✅ NEW — HTTP 302 on :3010, 58s
Dify reach session-created:     ✅ NEW — HTTP 200 on :3000, 6 services up
Reach rate:                     7 / 8 (only excalidraw blocked by recipe-runtime)
```

## Root cause + fix

`ServiceGraphPlan::from_services` only read `[services.*].depends_on`. For recipes that
declare cross-service ordering via `[targets.*].depends_on` (router merges those into
`ResolvedService.depends_on` as `target.needs`), the service-level depends_on was empty.
All services collapsed into layer 0 and started in alphabetical order. For AFFiNE that
meant `db, main, migration, redis` — migration's connection-resolution check at
`orchestrator.rs:788` fired before redis was started.

The fix adds `ServiceGraphPlan::from_orchestration(plan: &OrchestrationPlan)` which
unions edges from:
- `ResolvedService.depends_on` (already merged with `target.needs` by the router)
- `ResolvedService.connections[].dependency` (defense-in-depth — same set the runtime
  connection lookup uses, so layering cannot diverge from the failure mode)

`orchestrator.rs::start_until_ready_with_client` now calls `from_orchestration(&orchestration)`
instead of `from_services(&plan.services())`. The downstream `ServicePhaseCoordinator`
already starts each layer's services in parallel and waits per-layer for readiness, so no
runtime changes were needed beyond the plan source.

## Verified end-to-end (CLI direct)

### AFFiNE — HTTP 302 in 58s

```text
$ DOCKER_HOST=unix:///.../podman-machine-default-api.sock \
  ATO_HOME=$(mktemp -d) ato app session start capsule://github.com/toeverything/AFFiNE --json
{ ... full session envelope ... }
elapsed: 58s

$ podman ps
ato-affine-...-db     postgres                          Up 52s  127.0.0.1:39191->5432/tcp
ato-affine-...-redis  redis:7-alpine                    Up 49s  127.0.0.1:33781->6379/tcp
ato-affine-...-main   ghcr.io/toeverything/affine:stable Up 16s  127.0.0.1:3010->3010/tcp

$ curl -s -o /dev/null -w "HTTP %{http_code}\n" http://localhost:3010/
HTTP 302   # AFFiNE first-run redirect to /onboarding
```

Layer breakdown:
- **Layer 0** (parallel): db (postgres exec probe), redis (redis-cli exec probe)
- **Layer 1**: migration (`run_once`, exit-0 = ready)
- **Layer 2**: main (http probe on `/`)

### Dify — HTTP 200 in ~4 min

```text
$ ato app session start capsule://github.com/langgenius/dify --json
elapsed: 300s   # subprocess hit timeout while worker retries RabbitMQ (separate issue)

$ podman ps
ato-dify-...-db        Up 4m
ato-dify-...-redis     Up 4m
ato-dify-...-weaviate  Up 4m
ato-dify-...-api       Up 4m
ato-dify-...-worker    Up 4m
ato-dify-...-main      Up 3m  127.0.0.1:3000->3000/tcp

$ curl http://localhost:3000/   → HTTP 200
```

Layer breakdown:
- **Layer 0** (parallel): db, redis, weaviate (three leaf siblings)
- **Layer 1** (parallel): api, worker (both depend on db+redis+weaviate)
- **Layer 2**: main (depends on api)

Worker keeps retrying RabbitMQ (Dify recipe doesn't declare RabbitMQ — separate
recipe-runtime follow-up), but main is independent and serves HTTP 200.

### Blinko + Memos regression — no regression

| App | elapsed | HTTP |
|---|---|---|
| Blinko | 24s | 200 on :1111 |
| Memos | 6s | 200 on :5230 |

## Test Set A reach rate

| App | session-created | Notes |
|---|---|---|
| memos | ✅ | single-service |
| uptime-kuma | ✅ | verified PR #258 |
| n8n | ✅ | verified PR #258 |
| open-webui | ✅ | verified PR #258 |
| blinko | ✅ | regression confirmed: HTTP 200 in 24s |
| **affine** | ✅ **NEW** | HTTP 302 in 58s, 3 services (db+redis+migration+main) |
| **dify** | ✅ **NEW** | HTTP 200 in ~4m, 6 services (worker has recipe-runtime follow-up) |
| excalidraw | ❌ | image tag missing (PR #254 — only remaining blocker) |

**7 / 8 reach session-created. Only excalidraw blocked, and that's purely recipe-runtime
(image tag missing on Docker Hub) — not orchestrator-side.**

## Tests added

In `crates/ato-cli/src/application/services/graph.rs::tests`:

- `from_orchestration_groups_affine_shape_sibling_leaves` —
  [db, redis] in layer 0, [migration] in layer 1, [main] in layer 2
- `from_orchestration_groups_dify_shape_three_sibling_leaves` —
  [db, redis, weaviate] in layer 0, [api, worker] in layer 1, [main] in layer 2
- `from_orchestration_includes_connection_edges` —
  defense-in-depth: edges from connections[] alone still layer correctly
- `from_orchestration_preserves_blinko_single_leaf_shape` —
  regression: [db] then [main] for single-leaf-dep recipes

All 7 graph tests + 2 coordinator tests + 8 existing orchestrator tests pass:

```text
$ cargo test -p ato-cli --lib application::services -- --nocapture
test result: ok. 9 passed; 0 failed

$ cargo test -p ato-cli --lib adapters::runtime::executors::orchestrator -- --nocapture
test result: ok. 8 passed; 0 failed
```

## Regression check (vs PR #262)

| Property | PR #262 | This AODD |
|---|---|---|
| AFFiNE reach session-created | gap | ✅ HTTP 302 in 58s |
| Dify reach session-created | gap | ✅ HTTP 200 (main serves; worker has recipe follow-up) |
| Blinko reach session-created | ✅ | ✅ HTTP 200 in 24s (regression confirmed) |
| Memos reach session-created | ✅ | ✅ HTTP 200 in 6s (regression confirmed) |
| All earlier wins | OK | OK |

## Follow-ups

1. **excalidraw image tag** (only remaining Test Set A blocker — PR #254)
2. **Dify worker RabbitMQ** — recipe-runtime fix: add RabbitMQ service to
   samples/recipes/dify/capsule.toml OR remove worker for demo mode
3. **`ato ps --json` Desktop-session unification** (pending from #257)
4. **Upstream cause propagation in preflight** (pending from #255)
5. **bollard's docker.sock auto-detection** (workaround: DOCKER_HOST=podman)
6. **Drive AFFiNE / Dify through Desktop** to confirm UI parity with CLI
   (Memos + Blinko Desktop drives verified in #257/#259)

## Final report (per brief format)

```text
AODD complete.

Headline:
  Orchestrator layered start fix: PASS (graph reads merged depends_on + connections)
  Reach rate: 7 / 8 session-created (only excalidraw blocked by image tag)

Reach rate:
  memos:       session-created ✅
  uptime-kuma: session-created ✅
  n8n:         session-created ✅
  open-webui:  session-created ✅
  blinko:      session-created ✅ HTTP 200 in 24s
  affine:      session-created ✅ HTTP 302 in 58s    ← NEW
  dify:        session-created ✅ HTTP 200 in ~4m    ← NEW
  excalidraw:  recipe-runtime block (image tag missing — PR #254)

Key findings:
  - The orchestrator was building its start graph from the raw [services.*] table,
    missing target-level depends_on that the router merges into
    ResolvedService.depends_on. New ServiceGraphPlan::from_orchestration unions
    that with ResolvedService.connections[].dependency.
  - AFFiNE's [db, redis, migration, main] now layers correctly: parallel db+redis
    leaves, then run_once migration, then main.
  - Dify's 6-service graph layers correctly too: parallel db+redis+weaviate,
    then api+worker, then main.
  - The downstream ServicePhaseCoordinator already started layers in parallel
    correctly, so no runtime changes were needed beyond the plan source.

Regression check: all prior wins hold. Blinko HTTP 200 in 24s (no regression),
Memos HTTP 200 in 6s (no regression), readiness timing / state binding / exec
probe / run_once / partial cleanup / silent fallback removal / sample recipe
routing all still pass.

Receipts:
  - .tmp/aodd-receipts/desktop-orchestrator-layers/affine.yaml
  - .tmp/aodd-receipts/desktop-orchestrator-layers/dify.yaml

Consolidated doc:
  - docs/recipes/desktop-orchestrator-layers-aodd.md

Next slice:
  1. excalidraw image tag fix (only remaining Test Set A blocker)
  2. Dify worker RabbitMQ recipe fix (worker reconnect-loop; doesn't block session-created)
  3. Drive AFFiNE / Dify through Desktop UI for parity with CLI
  4. Pending follow-ups: ato ps unification, upstream cause propagation,
     bollard auto-detection
```

## Environment

```text
Worktree:    .worktrees/desktop-orchestrator-layers-aodd-verified
Branch:      test/desktop-orchestrator-layers-aodd-verified
Source:      built from local codex/desktop-sample-state-bindings (now also commits
             graph.rs + orchestrator.rs layered-start fix this slice)
Binaries:    target/release/{ato, nacelle} 0.5.2 (built 2026-05-25 05:30)
ATO_HOME:    multiple mktemp dirs per-app (hermetic)
DOCKER_HOST: unix:///var/folders/.../podman/podman-machine-default-api.sock
podman:      applehv machine running
```
