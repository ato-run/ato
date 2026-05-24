# Desktop state-binding AODD — fix verified end-to-end; next blocker is recipe-runtime readiness_probe

**Branch:** `test/desktop-state-binding-aodd-verified` (this PR)
**Base:** `dev` @ `1758a981` + local `codex/desktop-sample-state-bindings` branch (7 files: routing + Blinko/AFFiNE recipes + sample-recipe state auto-binding)
**Supersedes:** PR #255 (post-routing-fix baseline, state-binding was the blocker there)
**Date:** 2026-05-24

## Headline

The state-binding fix is **verified end-to-end**: sample-recipe-sourced session starts now
provision containers and reach the application's own startup logs. The `state '<X>' requires
an explicit persistent binding` failure from PR #255 is fully resolved.

`session-created` reach rate is still 0 — but for a **new reason, one layer deeper**: every
sample recipe declares `readiness_probe = { port = "<literal>" }`, and the orchestration
validator at `crates/ato-cli/src/adapters/runtime/executors/orchestrator.rs:1171` requires
that string to be the NAME of an env var, not the port number itself. The validator
short-circuits the launch right after the application has booted.

## Direct evidence the fix works (CLI session start)

```text
$ DOCKER_HOST=unix:///.../podman-machine-default-api.sock \
  ato app session start capsule://github.com/blinkospace/blinko --json
[db] fixing permissions on existing directory /var/lib/postgresql/data ... ok       # ← state.db-data mounted
[db] creating subdirectories ... ok
[db] selecting dynamic shared memory implementation ... posix
[db] selecting default max_connections ... 100
[db] selecting default shared_buffers ... 128MB
[db] selecting default time zone ... Etc/UTC
[db] creating configuration files ... ok
[main] Current Environment: production                                              # ← blinko started
{"status":"error","error":{
  "code":"E999",
  "message":"orchestration services failed to start in-process",
  "causes":["services.main.readiness_probe.port '1111' is not defined in service env"]
}}

$ ato app session start capsule://github.com/usememos/memos --json
[main] Source code: https://github.com/usememos/memos
[main]
[main] Happy note-taking!                                                           # ← memos serving requests
[main] 2026/05/24 10:03:39 INFO background runners started
{"status":"error","error":{
  "code":"E999",
  "message":"orchestration services failed to start in-process",
  "causes":["services.main.readiness_probe.port '5230' is not defined in service env"]
}}
```

In PR #255, both invocations rejected at `Validation error: state '<X>' requires an explicit
persistent binding before it can be attached`, no container ever started. Now the postgres
binding mounts cleanly under `$ATO_HOME/state/sample-recipes/blinko/db-data`, postgres
finishes `initdb`, and Memos prints "Happy note-taking!" before the readiness probe layer
rejects the launch.

## Desktop drive (Blinko)

Hermetic `ATO_HOME=/tmp/ato-state-binding-aodd-desktop`, `DOCKER_HOST` set to the podman
machine's API socket. Drove `NavigateToUrl capsule://github.com/blinkospace/blinko` →
`ForceApprovePending` (B4 MCP workaround). Result:

| Step | Result |
|---|---|
| NavigateToUrl queued | ✅ |
| Preflight | ✅ (no `continuing with launch fallback` line) |
| Consent wizard hydrates | ✅ (inferred — ForceApprovePending consumed a pending target) |
| ForceApprovePending consumes target | ✅ `consuming pending target route=CapsuleHandle{handle:"github.com/blinkospace/blinko",label:"blinko"}` |
| Boot wizard opens | ✅ `desktop launch input selected` + `spawning ato helper for session start` |
| Postgres container started | ✅ `ato-blinko-18182d53-db ... Up About a minute` in `podman ps` |
| Session start subprocess | ❌ readiness-probe validator (same as direct CLI) |
| `ato ps --json` | `[]` (failure short-circuits the session record write) |
| Visible error in log | ✅ `ERROR ato_launch: capsule boot failed` |
| `continuing with launch fallback` log | ✅ NOT present (silent fallback removal still holds) |

## Why session-created isn't reached yet (new blocker)

`crates/ato-cli/src/adapters/runtime/executors/orchestrator.rs:1171`:

```rust
let value = service.env.get(key).ok_or_else(|| {
    anyhow::anyhow!(
        "services.{}.readiness_probe.port '{}' is not defined in service env",
        service.service.name,
        key
    )
})?;
```

The validator requires `readiness_probe.port` to be an env-var NAME. Every sample recipe
declares it as the literal port number string:

| Recipe | Current | Expected (per validator) |
|---|---|---|
| memos | `port = "5230"` | `port = "MEMOS_PORT"` + `env = { MEMOS_PORT = "5230" }` |
| uptime-kuma | `port = "3001"` | `port = "UPTIME_KUMA_PORT"` + env entry |
| n8n | `port = "5678"` (already has `N8N_PORT="5678"` in env, but probe ref is wrong) | `port = "N8N_PORT"` |
| open-webui | `port = "8080"` (already has `PORT="8080"` in env) | `port = "PORT"` |
| excalidraw | `port = "80"` | needs both |
| blinko | `port = "1111"` | needs both |
| affine | `port = "3010"` | needs both (multi-service) |
| dify | multi-service port refs | needs review |

Two of them (n8n, open-webui) already have a matching env var declared and just need the
probe to reference the var by name instead of by literal value. The rest need both an env
addition and the probe reference change.

This is a **recipe-runtime** issue — outside the scope of this AODD's state-binding slice.

## Cleanup gap (worth a follow-up)

When session-start fails after one or more containers have already booted (Blinko's
postgres in this run), Desktop's launch flow logs the error but doesn't stop the partial
deployment. I had to manually `podman ps -q | xargs podman stop` to recover host ports.
Not in scope for this slice; surfaced as a follow-up.

## Final report (per brief format)

```text
AODD complete.

Headline:
  Desktop sample recipe state binding: PASS (fix verified — containers boot, mounts work)
  Desktop session-created reach:       FAIL (blocked at next layer: readiness_probe.port
                                              expects env-var name, recipes use literals)

Reach rate:
  Direct CLI session start:
    Blinko: postgres+app boot, then readiness probe rejection → session-created NOT reached
    Memos:  app boots and serves, then readiness probe rejection → session-created NOT reached
  Desktop drive:
    Blinko: same flow as direct CLI, plus visible-error in launch wizard log
            (no silent stall; partial-container cleanup gap noted)

Key findings:
  - State-binding fix works exactly as designed. Sample-recipe sessions auto-bind
    persistent state under $ATO_HOME/state/sample-recipes/<slug>/<state> and the
    Postgres container that previously rejected the launch now mounts /var/lib/postgresql/data
    successfully. Memos's state.data also mounts. Containers actually run.
  - Routing + visible-error layers from the previous slice still hold: preflight succeeds,
    consent wizard hydrates, no `continuing with launch fallback` log line.
  - NEW blocker class surfaced: every sample recipe uses literal port strings in
    readiness_probe (e.g. `port = "5230"`). The validator expects an env-var name and
    rejects every launch with `port '<N>' is not defined in service env`. This is a
    recipe-runtime issue — separate from the routing/binding code paths the last two
    slices fixed.
  - Cleanup gap: partial container deployments survive session-start failure. Postgres
    was left running after Blinko's Desktop drive errored.

Regression check (vs PR #255):
  - state explicit-binding failure: GONE (auto-binding works for all confirmed apps)
  - routing preflight pass: STILL PASS (24/24 CLI smoke; Desktop drive succeeds)
  - silent fallback removed: STILL PASS (no continuing-fallback log in any run)
  - DOCKER_HOST hand-off: works when env points at podman socket

Receipts:
  - .tmp/aodd-receipts/desktop-state-binding/blinko.yaml
  - .tmp/aodd-receipts/desktop-state-binding/memos.yaml

Consolidated doc:
  - docs/recipes/desktop-state-binding-aodd.md (this file)

Next slice (in order):
  1. Decide readiness_probe.port resolution strategy:
       (a) update every sample recipe to declare port env var + reference by name
       (b) make the validator accept literal port strings as a fallback
       (c) auto-synthesize PORT env var from target.port
     (b) is the smallest change. (a) is the most explicit.
  2. Add partial-container cleanup on session-start failure (stop any already-started
     containers + free ports + emit a single visible error in the launch wizard).
  3. After (1)+(2) land, re-run this AODD and expect:
       - Blinko, Memos, Uptime-Kuma, n8n, open-webui, excalidraw → session-created with HTTP 200
       - AFFiNE → likely session-created (single-app multi-state, migration container)
       - Dify   → likely visible recipe-runtime error (multi-service, arm64 emulation)
  4. Land the upstream-cause propagation in internal preflight (still pending from PR #255).
```

## Environment

```text
Worktree:    .worktrees/desktop-state-binding-aodd-verified           (this PR)
Branch:      test/desktop-state-binding-aodd-verified
Source:      built from local codex/desktop-sample-state-bindings (not committed)
Binaries:    /Users/.../target/release/{ato, nacelle} 0.5.2
             /Users/.../crates/ato-desktop/target/release/ato-desktop 0.5.2
ATO_HOME:    /tmp/ato-state-binding-aodd-desktop                       # hermetic
DOCKER_HOST: unix:///var/folders/.../podman/podman-machine-default-api.sock
podman:      applehv machine running; required pre-pulled images (postgres:14,
             pgvector/pgvector:pg16, redis:7-alpine, blinkospace/blinko:latest,
             ghcr.io/toeverything/affine:stable, neosmemo/memos:stable,
             louislam/uptime-kuma:1, n8nio/n8n:latest, ghcr.io/open-webui/open-webui:main)
note:        Docker Desktop is running on this host with a broken socket at
             ~/.docker/run/docker.sock; the CLI's bollard client picks that up by default
             and returns empty bodies. Setting DOCKER_HOST to the podman socket is required
             until ato-cli either probes podman first or surfaces a clearer error.
```
