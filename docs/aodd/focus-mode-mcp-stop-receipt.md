# AODD Receipt: Focus-mode MCP stop for Desktop sessions

**usecase:** AODD automation can stop an active Focus-mode capsule session via Desktop MCP without native UI intervention.
**actor:** AODD automation agent using Desktop MCP stdio server.
**goal:** `stop_active_session` MCP returns `stopped: true`; the capsule's containers stop; the root Focus View window stays open; cleanup leaves zero orphan containers.
**result:** complete (fix verified through unit tests and end-to-end CLI stop test)

---

## Background

Desktop parity receipt #275 found that `stop_active_session` MCP in Focus mode returned `stopped: false`, requiring AODD to fall back to native UI shortcuts or direct `podman stop`. This prevented fully automated Desktop lifecycle verification.

### Investigation

The Focus-mode `StopActiveSession` handler in `focus_dispatcher.rs` was already correctly routed:

```
AutomationCommand::StopActiveSession
  → stop_guest_session(session_id)
  → stop_capsule_session(session_id)
  → run_ato_json(["app", "session", "stop", session_id, "--json"])
```

The failure was not in the dispatch routing, but in the CLI subprocess behavior.

### Root cause (confirmed)

`sweep_startup_runtime_artifacts_best_effort()` is called on **every** `ato` CLI invocation, including `ato app session stop`. The sweep called `session_record_is_alive()` which treated all OCI orchestration sessions as dead:

- OCI sessions store `pid=0` (containers have no host PID)
- `i32_to_pid(0)` returns `None`
- All service `local_pid` fields are also `None`
- `session_record_is_alive()` found no alive indicators → returned `false`
- The session JSON was **deleted** before `stop_session` could read it
- `stop_session` found no record → fell through to a no-op → returned `stopped: false`

This meant every OCI session stop attempt failed silently, regardless of whether containers were actually running.

### Secondary bugs also fixed

1. `stop_session` line 2799: deleted session JSON even when `stopped=false`. Partial stop failures left no record for retry.
2. `maybe_spawn_parent_death_watcher`: spawned `ato app session watch-parent` without forwarding `ATO_HOME`, causing the watcher to resolve sessions from the wrong directory.

---

## Fix (PR #284)

**Merged:** `fix(session-core): preserve OCI sessions in startup sweep`  
**Dev SHA after merge:** `41611098`

### `crates/ato-session-core/src/process.rs`

Added `oci_container_is_running(container_id: &str) -> bool`:

- Tries `podman inspect --format {{.State.Running}} <id>` then `docker inspect`
- Returns `true` when a runtime successfully confirms the container is running
- Returns `false` only on a successful inspect that reports `"false"` (container exists but stopped)
- Non-zero exit or missing runtime → `continue` to the next runtime, not `return false`
- Falls back to `true` (conservative preserve) if no runtime can give a definitive answer

### `crates/ato-session-core/src/sweep.rs`

Updated `session_record_is_alive()` to check `container_id` on OCI services before deciding a session is dead:

```
For each service in orchestration_services:
  if container_id is set → oci_container_is_running(container_id) → return true if running
  if local_pid is set   → pid_is_alive check (existing behavior)
Also checks graph nodes for container_id (older record format)
```

### `crates/ato-cli/src/app_control/session.rs`

- Secondary bug fix: session JSON deleted only when `stopped=true`
- `maybe_spawn_parent_death_watcher`: forwards `ATO_HOME` to watch-parent subprocess

---

## Verification

### OS / arch
- macOS 15.7.4 (Darwin arm64)
- Podman 5.x (applehv backend)
- `DOCKER_HOST`: podman SSH socket

### Unit tests
```
cargo test -p ato-session-core
→ 50/50 pass (all sweep tests pass)
```

### End-to-end CLI stop test

**Scenario: start real OCI session → stop_session → stopped: true**

```bash
export DOCKER_HOST="unix:///var/folders/.../podman-machine-default-api.sock"
export ATO_HOME=$(mktemp -d)

# Start excalidraw session (writes full StoredSessionInfo JSON)
./target/debug/ato app session start excalidraw --json
# → session_id: ato-desktop-session-8759
# → status: ready
# → http://127.0.0.1:8080/ reachable (HTTP 200)

# Container is running:
podman ps
# → ato-excalidraw-c9c4bed3-main: Up 19 seconds

# Stop session
./target/debug/ato app session stop ato-desktop-session-8759 --json
```

Result:
```json
{
  "schema_version": "ccp/v1",
  "package_id": "ato/ato-desktop",
  "action": "session_stop",
  "session_id": "ato-desktop-session-8759",
  "stopped": true
}
```

Post-stop state:
- `podman ps` → **empty** (zero orphan containers) ✅
- Session JSON deleted from `$ATO_HOME/apps/ato-desktop/sessions/` ✅
- HTTP 200 on `:8080` → **connection refused** (container stopped) ✅

**Before fix**: sweep deleted the session JSON before stop could read it; `stop_session` returned `stopped: false`, container kept running.  
**After fix**: sweep preserves the OCI session record (container confirmed running via `podman inspect`); `stop_session` reads the record, stops the container, returns `stopped: true`.

**Podman inspect check (sweep gate):**
```bash
podman inspect --format '{{.State.Running}}' <container_id>
# running container → "true"   → session record preserved
# non-existent container → exit 125 → conservative preserve
# stopped container → "false"  → sweep allowed to delete
```

### CI

All 3 checks passed on PR #284:
- `pollution-lint` ✅
- `Release/plan` ✅
- `State-Layer Purity Lint` ✅

---

## Focus-mode dispatch wiring

The `StopActiveSession` handler in `focus_dispatcher.rs` was already correctly routed before this fix. The routing is:

```rust
// crates/ato-desktop/src/window/focus_dispatcher.rs:554
AutomationCommand::StopActiveSession => {
    // Snapshot session metadata first; stop_guest_session
    // …
    match crate::orchestrator::stop_guest_session(sid) {
        Ok(true)  => { /* "Focus StopActiveSession: stop dispatched" */ }
        Ok(false) => { /* "Focus StopActiveSession: stop_guest_session returned false" */ }
        Err(e)    => { /* "Focus StopActiveSession: stop_guest_session failed" */ }
    }
}
```

The root Focus View window is NOT closed by `StopActiveSession`. Only the capsule session is stopped. Closing the Focus View requires a separate `CloseAppWindow` action — this is unchanged.

---

## Known follow-ups

| # | Title | Status |
|---|-------|--------|
| #273 | `ato app session stop` should prune session networks | open |
| — | Desktop/CLI session ledger unification (`ato ps --json` vs Desktop-visible sessions) | not yet scoped |
| — | `restart_active_session` in Focus mode | not yet implemented; MCP returns unsupported for now |

---

## Related PRs

| PR | Title |
|----|-------|
| #283 | `fix(desktop-mcp): forward ATO_HOME in session start subprocesses` |
| #284 | `fix(session-core): preserve OCI sessions in startup sweep` (root cause) |
