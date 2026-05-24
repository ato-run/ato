# Desktop multi-leaf dep AODD — fix landed in oci_multi_service.rs but AFFiNE uses orchestrator.rs

**Branch:** `test/desktop-multileaf-aodd-verified` (this PR)
**Base:** `dev` @ `1758a981` + local `codex/desktop-sample-state-bindings` (now adds layered start in oci_multi_service.rs)
**Supersedes:** PR #260
**Date:** 2026-05-25

## Headline

The multi-leaf dep fix landed in `oci_multi_service.rs` (with passing unit tests for AFFiNE-shape
and Dify-shape dependency layers) but **AFFiNE and Dify go through a different executor
(`orchestrator.rs`) which wasn't updated**. The new layered-start code path is unreachable
from the in-process orchestration that all 3 multi-service Test Set A apps actually use.

AFFiNE still fails at the exact same error as PR #260:
```text
dependency 'redis' for service 'migration' has not been started
```

Same error message, same line: `crates/ato-cli/src/adapters/runtime/executors/orchestrator.rs:788`.

**Test Set A reach rate: still 5 / 8** (no regression; no progress).

## Direct evidence

```text
$ ato app session start capsule://github.com/toeverything/AFFiNE --json
[db] LOG:  listening on IPv6 address "::", port 5432
[db] LOG:  database system is ready to accept connections                        ← run_once + exec-probe wins still hold
{"error":{"cause":"dependency 'redis' for service 'migration' has not been started"}}
elapsed: 20s
```

Source of error:
```rust
// crates/ato-cli/src/adapters/runtime/executors/orchestrator.rs:785-792
for connection in &service.connections {
    let dependency = running.get(&connection.dependency).ok_or_else(|| {
        anyhow::anyhow!(
            "dependency '{}' for service '{}' has not been started",
            connection.dependency,
            service.name
        )
    })?;
    ...
}
```

When the orchestrator prepared `migration`, it walked the connections list and discovered
`redis` wasn't in the `running` map. `redis` was never started because the
`startup_order` iteration at lines 1032 and 1144 walks services one at a time and only
starts the next service when the previous one is ready — without considering whether
multiple sibling leaves could start in parallel.

## Two orchestration executors

`crates/ato-cli/src/adapters/runtime/executors/` has multiple executors:

| Executor | Path | Used by |
|---|---|---|
| `orchestrator.rs` | in-process supervisor; iterates `startup_order` one at a time (line 1032) | **AFFiNE, Blinko, Dify** (all multi-service OCI) via `start_orchestration_session_in_process` |
| `oci_multi_service.rs` | new layered-start path; just got the multi-leaf fix | NOT YET reached from AFFiNE/Blinko/Dify based on this run |
| `web_services.rs` | web-target specialization | recipes with `runtime = "web"` |
| `install_sh_runner.rs` | install.sh recipes | install.sh-flavored recipes |
| `oci_compose_runner.rs` | compose recipes | compose-flavored recipes |

The user's fix added passing unit tests for AFFiNE-shape / Dify-shape dependency layers in
`oci_multi_service.rs::tests` — those tests are correct, but the runtime dispatch for
AFFiNE/Dify doesn't route into that executor.

## What needs to happen

Apply the same layered-start logic to `orchestrator.rs`'s `startup_order` iteration:

1. Build dependency layers from the orchestration plan (same algorithm as
   `oci_multi_service.rs`'s new code).
2. At each layer, start ALL services in parallel.
3. Wait for the layer's readiness (db's pg_isready + redis's redis-cli ping) before
   advancing.
4. For `run_once` services in a layer, wait for exit-0 before advancing.

Alternative: route AFFiNE/Dify/Blinko through `oci_multi_service.rs` instead — but that's a
larger dispatch change.

## Reach rate

| App | session-created | Reason |
|---|---|---|
| memos / uptime-kuma / n8n / open-webui | ✅ | single-service, verified PR #257/#258 |
| blinko | ✅ | works via orchestrator.rs because `main` has only ONE leaf dep (db); HTTP 200 confirmed this run |
| excalidraw | ❌ | image tag missing (PR #254) |
| affine | ❌ | UNCHANGED — orchestrator.rs still serial start; redis never started |
| dify | ❌ | same |

**5 / 8 — no regression, no new pass.**

## Why Blinko works through the broken path

Blinko's `main` has exactly ONE `depends_on` entry (`db`). The orchestrator iterates
`[db, main]` in order:
1. Start db, wait for ready (pg_isready exec probe).
2. Start main, check connections. Main's only connection is to db; db is in `running` map. OK.

AFFiNE's `migration` has TWO `depends_on` entries (`db`, `redis`). The orchestrator iterates
`[db, redis, migration, main]` in declaration order:
1. Start db, wait for ready.
2. **Stop here** — line 1032's loop apparently exits / errors before reaching redis,
   OR migration's connection check fires when the orchestrator advances to migration
   without having started redis along the way.

(I haven't traced the exact dispatch but the error message places the failure at the
connection-resolution step for migration's `redis` connection.)

## Regression check

| Property | PR #260 | This AODD |
|---|---|---|
| AFFiNE multi-leaf dep start | gap | **STILL gap** (fix in wrong executor) |
| Blinko reach session-created | ✅ | ✅ HTTP 200, 18s elapsed |
| Memos session-created | ✅ | ✅ (verified earlier slices) |
| run_once recognition | OK | OK |
| exec-probe handling | OK | OK |
| All prior wins | OK | OK |

## Follow-ups

1. **Apply the multi-leaf layered-start to `orchestrator.rs`** (mirror the algorithm just
   landed in `oci_multi_service.rs::tests`). Unblocks AFFiNE + Dify.
2. **Alternative**: change the dispatch in `start_orchestration_session_in_process` so
   multi-service OCI recipes route through `oci_multi_service.rs` instead.
3. **excalidraw image tag** (PR #254)
4. **`ato ps --json` Desktop-session unification** (PR #257)
5. **Upstream cause propagation in preflight** (PR #255)
6. **bollard's docker.sock auto-detection**

## Final report (per brief format)

```text
AODD complete.

Headline:
  Multi-leaf dep fix in oci_multi_service.rs: tests PASS but the executor isn't
  reached by AFFiNE/Dify/Blinko. AFFiNE still errors at the same orchestrator.rs:788
  connection check ("dependency 'redis' for service 'migration' has not been started").
  Reach rate unchanged at 5 / 8.

Reach rate:
  memos / uptime-kuma / n8n / open-webui: session-created ✅
  blinko:     session-created ✅ (no regression, HTTP 200, 18s)
  excalidraw: image tag (PR #254)
  affine:     UNCHANGED — orchestrator.rs serial start, redis not started before migration
  dify:       UNCHANGED — same shape

Key findings:
  - Multi-leaf fix landed in oci_multi_service.rs with passing unit tests (AFFiNE-shape +
    Dify-shape) but that executor isn't on the runtime dispatch path for AFFiNE/Blinko/Dify.
  - The actual error site is orchestrator.rs:788 in start_orchestration_session_in_process.
  - orchestrator.rs's startup_order at line 1032 still iterates one service at a time.
  - Blinko works through the broken path because `main` has only ONE leaf dep (db).
    AFFiNE's `migration` has TWO leaf deps ([db, redis]) and that's where the gap fires.

Regression check (vs PR #260): all prior wins hold. No regression.

Receipts:
  - .tmp/aodd-receipts/desktop-multileaf/affine.yaml
  - .tmp/aodd-receipts/desktop-multileaf/blinko-regression.yaml

Consolidated doc:
  - docs/recipes/desktop-multileaf-aodd.md

Next slice:
  1. Apply the multi-leaf layered-start to orchestrator.rs (mirror oci_multi_service.rs).
     OR change dispatch to route multi-service OCI through oci_multi_service.rs.
  2. After (1), expect AFFiNE → session-created; Dify likely too (unless amd64 issues).
```

## Environment

```text
Worktree:    .worktrees/desktop-multileaf-aodd-verified
Branch:      test/desktop-multileaf-aodd-verified
Source:      built from local codex/desktop-sample-state-bindings (15 files modified)
Binaries:    target/release/{ato, nacelle} 0.5.2 (built 2026-05-25 05:10)
ATO_HOME:    multiple mktemp dirs per-app (hermetic)
DOCKER_HOST: unix:///var/folders/.../podman/podman-machine-default-api.sock
podman:      applehv machine running
```
