# Desktop readiness + cleanup AODD — Memos reaches session-created; Blinko cleanup verified

**Branch:** `test/desktop-readiness-cleanup-aodd-verified` (this PR)
**Base:** `dev` @ `1758a981` + local `codex/desktop-sample-state-bindings` branch (now 7-file diff: routing + sample-recipe state auto-binding + readiness-probe literal-port fallback + partial-OCI-start cleanup)
**Supersedes:** PR #256 (post-state-binding-fix baseline, readiness-probe + cleanup were the blockers there)
**Date:** 2026-05-24

## Headline

**Memos reaches session-created end-to-end via Desktop natural launch.** Container boots,
state mounts, serves HTTP 200, Desktop registers `session_id=ato-desktop-session-8019`, and
the WebView opens on `http://127.0.0.1:5230/`. **Brief's must-pass criterion met for Memos.**

Blinko's postgres readiness probe times out at 30s before `pg_isready` reports ready, so
session-start fails — BUT the new cleanup guard works perfectly: zero orphan containers,
zero orphan networks, postgres exits gracefully. This is the **regression fix** for PR
#256's orphan-postgres class.

## Wins this slice

| Property | PR #256 | This AODD |
|---|---|---|
| `readiness_probe.port` literal-string handling | rejected with E999 | **accepted as container port (env-placeholder still wins when matching env var exists)** |
| Memos container runs to readiness | never reached | **HTTP 200 on port 5230 (CLI direct + Desktop drive)** |
| Memos Desktop session registered | never reached | **session_id=ato-desktop-session-8019** |
| Memos WebView opens | never reached | **url=http://127.0.0.1:5230/ in WebView log** |
| Partial-container cleanup on session-start failure | orphan postgres left running | **clean shutdown: 0 orphan containers** |
| Routing + state binding (from prior slices) | OK | OK |
| Silent fallback removed (from prior slice) | OK | OK |

## Direct evidence

### CLI direct (Memos)

```text
$ DOCKER_HOST=unix:///.../podman-machine-default-api.sock \
  ATO_HOME=$(mktemp -d) ato app session start capsule://github.com/usememos/memos --json
{ ... full session envelope ... "schema_version": 2, "execution_id": "blake3:..." }

$ ATO_HOME=$ATO_HOME ato ps --json
[]                                                          # ← see follow-ups; Desktop session ledger is separate

$ podman ps
ato-memos-7e9d899d-main  docker.io/neosmemo/memos:stable  Up 13 seconds  127.0.0.1:5230->5230/tcp

$ curl -s -o /dev/null -w "HTTP %{http_code}\n" http://localhost:5230/
HTTP 200
```

### Desktop drive (Memos) — what changed

```text
INFO  Focus-mode NavigateToUrl url=capsule://github.com/usememos/memos
DEBUG calling ato internal preflight handle="capsule://github.com/usememos/memos"
INFO  ForceApprovePending: consuming pending target route=CapsuleHandle{handle:"github.com/usememos/memos",label:"memos"}
INFO  desktop launch input selected launch_input.kind="handle"
DEBUG spawning ato helper for session start handle="github.com/usememos/memos"
INFO  capsule session started session_id=ato-desktop-session-8019 handle="github.com/usememos/memos"           ← NEW (was ERROR in #256)
INFO  AppCapsuleShell: WebView created for running session handle=github.com/usememos/memos url=http://127.0.0.1:5230/ session_id=ato-desktop-session-8019  ← NEW
INFO  windowCloseBehavior=keep-session-running: sessions detached ids=[]
INFO  ato_launch: boot wizard closed                                                                              ← clean dismiss, no error state
```

`podman ps` after this drive:

```text
ato-memos-aef938b6-main  ...  Up 4 minutes  127.0.0.1:5230->5230/tcp     ← (kept running per windowCloseBehavior)
```

### Blinko cleanup verified

```text
INFO  capsule session started... (never reached)
ERROR ato session start failed handle="github.com/blinkospace/blinko"
      ...service 'db' readiness check timed out after 30s
ERROR ato_launch: capsule boot failed

# postgres exits cleanly:
[db] LOG:  background worker "logical replication launcher" (PID 65) exited with exit code 1
[db] LOG:  database system is shut down

# podman ps after failure: NO blinko containers (only the Memos session from earlier)
ato-memos-aef938b6-main  ...  Up 4 minutes
```

Compare to PR #256 where `ato-blinko-...-db` was left "Up About a minute" after failure and
host port 5432 was held until manual cleanup.

## Brief acceptance checklist (Memos)

- [x] consent wizard が開く
- [x] loading のまま止まらない
- [x] hydrated preview に capsule_id / capsule_version / requirements が出る (confirmed via CLI Phase 1 in PR #256, structural now)
- [x] preflight_failed=false
- [x] Approve が押せる
- [x] Approve 後に boot wizard へ進む
- [x] **session-created まで到達する** ← Memos
- [x] local_url / primary_url が取れる (url=http://127.0.0.1:5230/ in WebView log)
- [x] HTTP 200 または app-specific healthy response が返る (200 from `/`)

## Brief acceptance checklist (Blinko — visible-error path is acceptable per brief)

- [x] consent wizard が開く
- [x] loading のまま止まらない
- [x] preflight passes
- [x] approve consumed → boot wizard opens
- [ ] **session-created** — NOT REACHED (db readiness timeout)
- [x] visible actionable error (ERROR log; wizard closes with capsule-boot-failed)
- [x] silent stall しない
- [x] cleanup completed (no orphan containers/networks) ← THE PR #256 regression is fixed

## Follow-ups surfaced

These were noticed during the AODD; flagging them but not in this slice's scope:

1. **Blinko postgres probe timeout**: recipe declares `timeout_seconds = 60` for the
   `pg_isready` probe but the orchestrator times out at 30s. Investigate whether the
   orchestrator caps the probe timeout or whether the recipe's value isn't being threaded
   through.

2. **`ato ps --json` doesn't surface Desktop sessions**: Memos was running and serving via
   Desktop's `ato-desktop-session-8019` ledger, but `ato ps --json` returned `[]`. CLI
   operators have no way to see Desktop-launched sessions. Either unify the ledger or add
   a `--include-desktop` flag.

3. **Upstream cause propagation in preflight** (still pending from PR #255).

4. **`bollard` Docker socket auto-detection** — CLI defaults to `~/.docker/run/docker.sock`
   (Docker Desktop) even when it returns empty bodies. `DOCKER_HOST` must be set to point
   at the podman socket. Worth probing podman first or surfacing a clearer error.

## Final report (per brief format)

```text
AODD complete.

Headline:
  Desktop readiness probe + partial cleanup: PASS
  Memos session-created (must-pass):         PASS
  Blinko session-created:                    visible-error (db probe timeout, acceptable)

Reach rate:
  Direct CLI session start:
    Memos:  session-created ✅ HTTP 200 on :5230
    Blinko: db probe timeout, cleanup ✅
  Desktop drive:
    Memos:  session-created ✅ session_id=ato-desktop-session-8019, WebView open, HTTP 200
    Blinko: visible actionable error, cleanup ✅ (vs PR #256 orphan postgres)

Key findings:
  - readiness_probe.port literal-string fallback works. Memos's `port = "5230"` is now
    interpreted as the container port (5230) when no matching env var exists, and the
    OCI host-port mapping is preserved.
  - Partial-container cleanup works. Blinko's postgres exited gracefully and no host
    port stayed bound after the readiness failure.
  - Desktop session registration works. session_id=ato-desktop-session-8019 ledger entry
    + WebView creation + boot wizard closed cleanly = Memos reaches session-created by
    every brief criterion.
  - Memos kept running per windowCloseBehavior=keep-session-running policy. The Desktop
    drive's host port stays bound after the launch wizard closes (intended behavior).

Regression check (vs PR #256):
  - readiness_probe.port literal handling: PASS (was BLOCKER in #256)
  - partial-container cleanup: PASS (orphan postgres class is fixed)
  - silent fallback removed: STILL PASS
  - routing + state binding from prior slices: STILL PASS

Receipts:
  - .tmp/aodd-receipts/desktop-readiness-cleanup/memos.yaml
  - .tmp/aodd-receipts/desktop-readiness-cleanup/blinko.yaml

Consolidated doc:
  - docs/recipes/desktop-readiness-cleanup-aodd.md

Next slice candidates:
  1. Investigate Blinko postgres probe timeout cap (30s observed vs 60s declared).
     If fixed, Blinko should reach session-created too.
  2. Unify ato-cli ps and Desktop session ledgers (so `ato ps` shows Desktop sessions).
  3. Land upstream-cause propagation in internal preflight (still pending from #255).
  4. Drive uptime-kuma / n8n / open-webui / excalidraw through Desktop to measure
     full Test Set A reach rate now that readiness + cleanup are unblocked.
  5. Drive AFFiNE / Dify and decide which still need recipe-runtime work.
```

## Environment

```text
Worktree:    .worktrees/desktop-readiness-cleanup-aodd-verified
Branch:      test/desktop-readiness-cleanup-aodd-verified
Source:      built from local codex/desktop-sample-state-bindings (not committed; 7 files + 2 recipes)
Binaries:    target/release/{ato, nacelle} 0.5.2 (built 21:05)
             crates/ato-desktop/target/release/ato-desktop 0.5.2 (built 21:13)
ATO_HOME:    /tmp/ato-readiness-desktop-aodd + multiple mktemp dirs (all hermetic)
DOCKER_HOST: unix:///var/folders/.../podman/podman-machine-default-api.sock (required workaround)
podman:      applehv machine running; required images present from prior runs
```
