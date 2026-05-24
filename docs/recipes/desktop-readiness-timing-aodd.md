# Desktop readiness-timing AODD — 4/5 single-service apps reach session-created; new exec-probe gap exposed

**Branch:** `test/desktop-readiness-timing-aodd-verified` (this PR)
**Base:** `dev` @ `1758a981` + local `codex/desktop-sample-state-bindings` (now adds readiness-timing fix)
**Supersedes:** PR #257
**Date:** 2026-05-25

## Headline

The readiness-timing fix is verified: `orchestrator.rs` + `web_services.rs` now read
`initial_delay_seconds` / `timeout_seconds` / `interval_seconds` from the manifest probe
instead of using a hardcoded 30s cap. Blinko's "timed out after 30s" is now "timed out
after 60s" — recipe value honored.

**4 of 5 single-service Test Set A apps reach session-created via CLI direct.** Only
excalidraw is blocked, and only by a pre-existing recipe-runtime issue (image tag missing
on Docker Hub — first documented in PR #254).

Blinko surfaces a **new orchestrator-level bug**: `wait_until_ready_in_state` silently
no-ops `exec` readiness probes. The timing fix made the symptom visible (orchestrator now
patiently waits the full 60s instead of bailing at 30s), and a manual postgres smoke test
confirmed the actual bug is that `pg_isready` is never executed at all.

## Test Set A reach rate (CLI direct session start)

| App | Recipe probe | Container | HTTP | session-created | Notes |
|---|---|---|---|---|---|
| **memos** | `http_get = "/"` | up | 200 | ✅ | No regression vs PR #257 |
| **uptime-kuma** | `http_get = "/"` | up | 302 | ✅ | 302 → /setup is expected first-run |
| **n8n** | `http_get = "/healthz"` | up | 404 on `/` | ✅ | /healthz passes; n8n serves on /workflow paths |
| **open-webui** | no probe (by design) | up | 000 (timing) | ✅ | Recipe header notes first-run downloads ~30 model files; container running |
| **excalidraw** | `http_get = "/"` | **never started** | n/a | ❌ | Image tag `0.17.6` missing on Docker Hub (PR #254) |
| **blinko** | `exec = ["pg_isready", ...]` for db | db starts, main never | n/a | ❌ | **NEW**: orchestrator's wait loop no-ops exec probes |
| affine | exec + http (multi-service) | not tested | n/a | ❌ | Same exec-probe gap likely affects migration container |
| dify | http + multi-service | not tested | n/a | ❌ | Multi-service + arm64 emulation |

**Single-service reach rate: 4/5 PASS (80%).**
**Test Set A overall: 4/8 PASS, 1/8 recipe-runtime block, 3/8 affected by exec-probe gap or untested.**

## Wins this slice

| Property | PR #257 | This AODD |
|---|---|---|
| Blinko's db probe timeout | hardcoded 30s | **honors recipe's 60s** |
| Memos reach session-created | ✅ verified | ✅ confirmed (no regression) |
| **uptime-kuma** reach session-created | not measured | ✅ **NEW PASS** (HTTP 302) |
| **n8n** reach session-created | not measured | ✅ **NEW PASS** (HTTP 404 on /, 200 on /healthz) |
| **open-webui** reach session-created | not measured | ✅ **NEW PASS** (container + port, first-run downloading) |
| Partial-container cleanup | ✅ verified | ✅ confirmed (Blinko run leaves zero orphans) |
| Routing + state binding | ✅ | ✅ |
| Silent fallback removed | ✅ | ✅ |

## New finding: exec-probe silent no-op in orchestrator.rs

Root cause located in `crates/ato-cli/src/adapters/runtime/executors/orchestrator.rs`:

```rust
// line 868 wait_until_ready_in_state — the main wait loop:
if !uses_event_driven_readiness(&service) {
    if let Some(port) = resolve_probe_port(&service, &probe)? {       // ← exec ⇒ None
        if readiness_probe_ok(&probe, port)? {                         // ← never called for exec
            return Ok(());
        }
    }
}

// line 1175 resolve_probe_port — short-circuits for exec:
fn resolve_probe_port(service: &RunningService, probe: &ReadinessProbe) -> Result<Option<u16>> {
    if probe.exec.is_some() {
        return Ok(None);                                                // ← here
    }
    ...
}

// line 1296 readiness_probe_ok — only http_get + tcp_connect, no exec branch:
fn readiness_probe_ok(probe: &ReadinessProbe, port: u16) -> Result<bool> {
    if let Some(path) = probe.http_get... { return Ok(http_probe(...)); }
    if let Some(target) = probe.tcp_connect... { return Ok(tcp_probe(...)); }
    anyhow::bail!("readiness_probe must define http_get, tcp_connect, or exec");
    // ↑ bail is unreachable because port=None upstream skips the call entirely
}
```

For exec probes the loop sleeps `interval` repeatedly until `timeout_seconds` elapses but
never executes the exec command. `pg_isready` is never invoked.

`oci_multi_service.rs:976` has the correct pattern (`if let Some(cmd) = &probe.exec`), so
the fix is to mirror that in `orchestrator.rs`.

### Manual smoke test confirming postgres isn't slow

```text
$ podman run -d --name aodd-pg-probe-test \
    -e POSTGRES_DB=blinko -e POSTGRES_USER=blinko -e POSTGRES_PASSWORD=test \
    -p 15432:5432 postgres:14
$ podman exec aodd-pg-probe-test pg_isready -U blinko -d blinko
t=5s: /var/run/postgresql:5432 - accepting connections          ← ready in ~5s
```

So the 60s "timeout" is genuinely unused — postgres would have passed the probe within 5
seconds had `pg_isready` actually been invoked.

## Brief acceptance

```text
- CLI smoke が全対象で通る: PASS (24/24 from PR #255 still passes; new run confirms)
- Blinko の db probe が 30s ではなく recipe 通り 60s まで待つか: PASS (60s honored)

But also surfaced:
- Even at 60s, the exec probe is silently skipped → Blinko still doesn't reach session-created
- 4 of 5 single-service apps now reach session-created (memos/uptime-kuma/n8n/open-webui)
- excalidraw still blocked by pre-existing image-tag issue
```

## Follow-ups (not in this slice's scope)

1. **Implement exec-probe handling in orchestrator.rs's wait loop** (mirror
   `oci_multi_service.rs:976`). Unblocks Blinko + likely AFFiNE migration container + any
   future recipe that uses exec probes for db services.
2. **Easy recipe workaround for Blinko**: change `services.db.readiness_probe` from exec to
   `tcp_connect = "127.0.0.1", port = "5432"`. Postgres binds 5432 once it's listening, so
   the TCP probe (which IS implemented) would succeed shortly after `pg_isready` would have.
3. **Investigate readiness for open-webui**: recipe has no probe by design (model download
   takes too long). Session-start returns immediately after container creation. Worth
   confirming that's the intended UX (operator launches → wizard closes → wait for model
   download in browser).
4. **excalidraw image tag** (pre-existing — PR #254 Class A finding).
5. **`ato ps --json` doesn't surface Desktop sessions** (pending from #257).
6. **Upstream cause propagation in preflight** (pending from #255).
7. **bollard's docker.sock auto-detection** (workaround: DOCKER_HOST must point at podman).

## Final report (per brief format)

```text
AODD complete.

Headline:
  Readiness timing fix: PASS (recipe timeout_seconds=60 honored, was 30s cap)
  Single-service Test Set A reach rate: 4/5 session-created
  Blinko: NEW finding — exec-probe is silently no-op'd in orchestrator wait loop

Reach rate:
  memos:       session-created ✅ HTTP 200 on :5230
  uptime-kuma: session-created ✅ HTTP 302 on :3001 (expected first-run redirect)
  n8n:         session-created ✅ HTTP 200 on :5678/healthz (404 on / is app-specific)
  open-webui:  session-created ✅ port bound (first-run downloads ongoing; no probe by design)
  excalidraw:  recipe-runtime block (image tag 0.17.6 missing — PR #254)
  blinko:      visible-error (db exec probe never executed → 60s timeout)
  affine/dify: not tested this run (multi-service, same exec-probe gap likely)

Regression check (vs PR #257):
  - readiness_probe.port literal handling: PASS
  - partial-container cleanup: PASS
  - silent fallback removed: PASS
  - routing + state binding: PASS
  - memos session-created via CLI direct: PASS (no regression)

Receipts:
  - .tmp/aodd-receipts/desktop-readiness-timing/blinko.yaml
  - .tmp/aodd-receipts/desktop-readiness-timing/single-service-batch.yaml

Consolidated doc:
  - docs/recipes/desktop-readiness-timing-aodd.md

Next slice candidates:
  1. Implement exec-probe handling in orchestrator.rs wait loop (unblocks Blinko)
     OR change Blinko recipe to use tcp_connect probe on port 5432 as a workaround
  2. Drive uptime-kuma / n8n / open-webui through Desktop to confirm UI parity
  3. Drive AFFiNE / Dify to see how far multi-service goes after Blinko unblock
  4. Pending items: ato ps Desktop unification, upstream cause propagation,
     bollard docker.sock auto-detection
```

## Environment

```text
Worktree:    .worktrees/desktop-readiness-timing-aodd-verified
Branch:      test/desktop-readiness-timing-aodd-verified
Source:      built from local codex/desktop-sample-state-bindings (not committed; 7 files + 2 recipes)
Binaries:    target/release/{ato, nacelle} 0.5.2 (built 2026-05-25 02:58)
             crates/ato-desktop/target/release/ato-desktop 0.5.2 (built earlier 21:13)
ATO_HOME:    multiple mktemp dirs per-app (hermetic)
DOCKER_HOST: unix:///var/folders/.../podman/podman-machine-default-api.sock (workaround)
podman:      applehv machine running
```
