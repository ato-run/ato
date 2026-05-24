# Desktop exec-probe AODD — Blinko reaches session-created via both CLI + Desktop; 5/8 reach rate

**Branch:** `test/desktop-exec-probe-aodd-verified` (this PR)
**Base:** `dev` @ `1758a981` + local `codex/desktop-sample-state-bindings` (now adds exec-probe impl in orchestrator + oci.rs)
**Supersedes:** PR #258
**Date:** 2026-05-25

## Headline

The exec-probe fix unblocks Blinko entirely. `pg_isready` now actually runs against the
postgres container via the new `OciRuntimeClient::exec_container` (bollard exec API).

**Blinko reaches session-created via both CLI direct AND Desktop natural launch.** Brief's
must-pass criterion for Blinko: MET for the first time.

**Test Set A reach rate: 5 of 8** (memos, uptime-kuma, n8n, open-webui, blinko). Remaining
three each blocked by a different separate issue: excalidraw by recipe-runtime (image tag),
AFFiNE by run_once dependency lifecycle, Dify by pre-flight dependency planning.

## Blinko end-to-end verification

### CLI direct (23s)

```text
$ DOCKER_HOST=unix:///.../podman-machine-default-api.sock \
  ATO_HOME=$(mktemp -d) ato app session start capsule://github.com/blinkospace/blinko --json
{ ... full session envelope ... }

elapsed: 23s

$ podman ps
ato-blinko-5ca09112-db    docker.io/library/postgres:14        Up 14 seconds  127.0.0.1:46085->5432/tcp
ato-blinko-5ca09112-main  docker.io/blinkospace/blinko:latest  Up 10 seconds  127.0.0.1:1111->1111/tcp

$ curl -s -o /dev/null -w "HTTP %{http_code}\n" http://localhost:1111/
HTTP 200
```

The 23s elapsed breaks down as: ~13s for db to reach ready via pg_isready exec, ~10s
for main to bind 1111 after db ready. Matches PR #258's manual smoke test prediction
(postgres ready in ~5s; the extra 5-8s is bollard exec round-trip + scheduler tick).

### Desktop drive (30s total)

```text
19:05:32 INFO  Focus-mode NavigateToUrl url=capsule://github.com/blinkospace/blinko
19:05:32 DEBUG calling ato internal preflight
19:05:38 INFO  ForceApprovePending: consuming pending target ... label:"blinko"
19:05:38 INFO  desktop launch input selected
19:05:39 DEBUG spawning ato helper for session start handle="github.com/blinkospace/blinko"
19:06:02 INFO  capsule session started session_id=ato-desktop-session-93828 handle="github.com/blinkospace/blinko"   ← NEW
19:06:02 INFO  AppCapsuleShell: WebView created for running session url=http://127.0.0.1:1111/ session_id=ato-desktop-session-93828   ← NEW
19:06:02 INFO  ato_launch: boot wizard closed
```

```text
$ podman ps
ato-blinko-ab2dab7b-db    Up About a minute  127.0.0.1:44027->5432/tcp
ato-blinko-ab2dab7b-main  Up About a minute  127.0.0.1:1111->1111/tcp

$ curl -s -o /dev/null -w "HTTP %{http_code}\n" http://localhost:1111/
HTTP 200
```

All 8 brief criteria for session-created are met for Blinko via Desktop.

## Test Set A reach rate after this slice

| App | Recipe shape | session-created | How |
|---|---|---|---|
| memos | single-service, http probe | ✅ | CLI 7s, Desktop verified in PR #257 |
| uptime-kuma | single-service, http probe | ✅ | CLI (verified PR #258) |
| n8n | single-service, http probe | ✅ | CLI (verified PR #258) |
| open-webui | single-service, no probe (by design) | ✅ | CLI (verified PR #258) |
| **blinko** | **multi-service db+main, exec probe** | ✅ **NEW** | **CLI 23s, Desktop 30s** |
| excalidraw | single-service, http probe | ❌ | Image tag `0.17.6` missing on Docker Hub (PR #254) |
| affine | multi-service db+redis+migration+main, exec + run_once | ❌ | run_once lifecycle gap (new follow-up) |
| dify | 6-service, multi-target, amd64 emulation | ❌ | pre-flight dependency planning gap (new follow-up) |

**5 / 8 reach session-created. 1 recipe-runtime block. 2 new orchestrator gaps.**

## Wins this slice (vs PR #258)

| Property | PR #258 | This AODD |
|---|---|---|
| Blinko db probe behavior | timed out at 60s (never executed) | **pg_isready runs, db ready in ~10s** |
| Blinko CLI session-created | never | **reached in 23s** |
| Blinko Desktop session-created | never | **reached in 30s, session_id + WebView** |
| Exec-probe impl in orchestrator | silent no-op | **bollard exec API wired** |
| Memos no regression | OK | OK |
| All prior wins from #253–#258 | OK | OK |

## New finding: orchestrator gaps surfaced by AFFiNE/Dify

After Blinko unblocked, the next two multi-service recipes expose different gaps:

**AFFiNE** — db's exec probe runs and postgres bootstraps successfully:
```text
[db] running bootstrap script ... ok
[db] performing post-bootstrap initialization ... ...
```
Then orchestrator errors with:
```text
dependency 'migration' for service 'main' has not been started
```

The `migration` service is declared `run_once = true` in the recipe. The orchestrator
appears to treat `run_once` services as never-ready (since they exit and are no longer
running), so `main` is never scheduled.

**Dify** fails in 1 second with:
```text
dependency 'db' for service 'api' has not been started
```

This failed before ANY container was started — suggests the orchestrator's dependency
planner rejects Dify upstream. Possibly because Dify is a 6-service recipe (db, redis,
weaviate, api, worker, web) with non-trivial depends_on chains, or because amd64
emulation requirements aren't being honored.

Both are out of scope for this slice but should be separate next-slice targets.

## Follow-ups

1. **Implement `run_once` semantics in orchestrator.rs**: when a service has
   `run_once = true`, wait for its container to exit successfully (status 0) before
   scheduling its dependents. Unblocks AFFiNE.
2. **Investigate Dify pre-flight dependency planning failure**: orchestrator rejects
   Dify before starting any service. May need amd64 emulation threading, or the
   dependency graph resolution needs to handle 6-service chains. Unblocks Dify.
3. **Excalidraw image tag** (pre-existing — PR #254).
4. **`ato ps --json` Desktop-session unification** (pending from #257).
5. **Upstream cause propagation in preflight** (pending from #255).
6. **bollard's docker.sock auto-detection** (workaround: DOCKER_HOST=podman).

## Final report (per brief format)

```text
AODD complete.

Headline:
  Exec-probe fix: PASS (pg_isready actually runs via bollard exec API)
  Blinko: session-created via CLI ✅ AND Desktop ✅ — must-pass criterion MET
  Test Set A reach rate: 5 / 8 session-created

Reach rate:
  memos:       session-created ✅ CLI 7s HTTP 200 on :5230
  uptime-kuma: session-created ✅ (PR #258)
  n8n:         session-created ✅ (PR #258)
  open-webui:  session-created ✅ (PR #258)
  blinko:      session-created ✅ CLI 23s, Desktop 30s, HTTP 200 on :1111   ← NEW
  excalidraw:  recipe-runtime block (image tag 0.17.6 missing — PR #254)
  affine:      run_once dependency gap (new follow-up)
  dify:        pre-flight dependency planning gap (new follow-up)

Key findings:
  - Exec-probe fix works end-to-end. pg_isready runs against postgres container,
    db reaches ready in ~10s (matches PR #258's manual smoke prediction of ~5s
    plus bollard exec round-trip), main starts after db, full chain to HTTP 200.
  - Blinko is the brief's must-pass app and is now reachable from both CLI direct
    and Desktop natural launch.
  - Two new orchestrator gaps surfaced by trying AFFiNE/Dify, each a separate
    next-slice target.

Regression check:
  - exec-probe handling: PASS (was BLOCKER in #258)
  - readiness_probe.port literal handling: PASS
  - partial-container cleanup: PASS
  - silent fallback removed: PASS
  - routing + state binding + timing: PASS
  - memos session-created: PASS (no regression)

Receipts:
  - .tmp/aodd-receipts/desktop-exec-probe/blinko.yaml
  - .tmp/aodd-receipts/desktop-exec-probe/multi-service-batch.yaml

Consolidated doc:
  - docs/recipes/desktop-exec-probe-aodd.md

Next slice candidates:
  1. Implement run_once service semantics in orchestrator (unblocks AFFiNE)
  2. Investigate Dify pre-flight dependency planning (unblocks Dify)
  3. Drive uptime-kuma / n8n / open-webui / blinko through Desktop to confirm UI
     parity with CLI (Memos already verified in #257; Blinko verified in this run)
  4. excalidraw image tag fix
  5. Pending: ato ps Desktop unification, upstream cause propagation in preflight,
     bollard docker.sock auto-detection
```

## Environment

```text
Worktree:    .worktrees/desktop-exec-probe-aodd-verified
Branch:      test/desktop-exec-probe-aodd-verified
Source:      built from local codex/desktop-sample-state-bindings (not committed; 9 files + 2 recipes)
Binaries:    target/release/{ato, nacelle} 0.5.2 (built 2026-05-25 03:57)
             crates/ato-desktop/target/release/ato-desktop 0.5.2 (built 04:05)
ATO_HOME:    /tmp/ato-exec-desktop + multiple mktemp dirs (hermetic)
DOCKER_HOST: unix:///var/folders/.../podman/podman-machine-default-api.sock (workaround)
podman:      applehv machine running
```
