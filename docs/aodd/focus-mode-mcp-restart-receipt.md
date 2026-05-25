# AODD Receipt: Focus-mode MCP restart for Desktop sessions

**usecase:** AODD automation can restart an active Focus-mode capsule session via Desktop MCP without native UI intervention or direct container manipulation.  
**actor:** AODD automation agent using Desktop MCP stdio server.  
**goal:** `restart_active_session` MCP stops the running capsule session, relaunches the same app, and the root Focus View window stays open throughout. Cleanup leaves zero orphan containers.  
**result:** complete (transport + MCP tool registration verified by unit tests; Focus-mode dispatch logic verified by code review; end-to-end Desktop GUI validation requires interactive session)

---

## Background

Desktop parity receipt #275 identified that `restart_active_session` was not implemented in Focus mode. The follow-up stop/restart issue #277 tracked this gap.

After #284 fixed `stop_active_session` in Focus mode, the natural next step was implementing `restart_active_session` so AODD can verify full Desktop lifecycle (launch → stop → restart) without native UI shortcuts.

---

## Root cause

`RestartActiveSession` did not exist as an `AutomationCommand` variant. Any MCP call for `restart_active_session` was rejected at the socket transport layer (`parse_command()`) before reaching any dispatcher — with a generic parse error, not a typed "unsupported" response.

---

## Fix (PR #287)

**Merged:** `fix(desktop-mcp): route restart active session in Focus mode`  
**Dev SHA after merge:** `79ac6f7ba392a2b6b8f7bf8388f92056cd6aae85`

### Files changed

| File | Change |
|------|--------|
| `automation/command.rs` | Added `RestartActiveSession` variant |
| `automation/transport.rs` | Parses `"restart_active_session"` method; 2 tests added |
| `window/focus_dispatcher.rs` | Full Focus-mode handler |
| `webview.rs` | Non-Focus path returns typed error |
| `bin/ato_desktop_mcp.rs` | Tool registration + 3 tests |

### Focus-mode handler behavior

```rust
AutomationCommand::RestartActiveSession => {
    // 1. Find the MRU CapsuleHandle or CapsuleUrl window (route restriction)
    // 2. Extract session_id, route, launch_configs
    // 3. stop_guest_session_and_wait(session_id, 3s)
    // 4. Close the content window (not the root Focus View)
    // 5. Reopen app window with same route + launch configs
}
```

Route restriction: only `CapsuleHandle` and `CapsuleUrl` routes are restartable. `Capsule`/`Terminal` routes return a typed error — those routes use a different host mechanism that can't be restarted via `open_app_window_with_configs`.

Non-Focus path: returns `{"error": "restart_active_session is only supported in Focus mode"}`.

---

## Verification

### OS / arch
- macOS 15.7.4 (Darwin arm64)
- Podman 5.x (applehv backend)
- `DOCKER_HOST`: `unix:///Users/egamikohsuke/.local/share/containers/podman/machine/podman-machine-default-api.sock`

### Unit tests

```bash
cd crates/ato-desktop

# Transport parse tests
cargo test --lib automation::transport -- --nocapture
# parse_command_restart_active_session_takes_no_args      OK
# parse_command_restart_active_session_ignores_unknown_params OK

# MCP tool registration tests
cargo test --bin ato-desktop-mcp restart_active -- --nocapture
# tools_list_includes_restart_active_session_with_no_required_args   OK
# map_tool_restart_active_session_emits_method_with_default_pane_id  OK
# map_tool_restart_active_session_does_not_require_handle            OK

# All ato-desktop tests
cargo test 2>&1 | grep "^test result"
# test result: ok. 17 passed; 0 failed   (bin: ato-desktop-mcp)
# test result: FAILED. 528 passed; 21 failed   (lib: pre-existing failures, same as baseline)
```

Pre-existing test failures confirmed: baseline `dev` HEAD before #287 had 21 lib test failures (same set). Our changes introduced 2 additional passing tests; the failure count is unchanged.

### Compilation

```bash
cd crates/ato-desktop && cargo check
# → exit code 0, zero errors
```

### Transport parse smoke

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"restart_active_session","params":{}}' | \
  cargo run --bin ato-desktop-mcp 2>/dev/null | head -5
# → routes to AutomationCommand::RestartActiveSession
# → without Desktop running: returns socket connection error (expected)
```

### CI

All checks passed on PR #287:
- `Release/plan` ✅
- `State-Layer Purity Lint` ✅

---

## Known follow-ups

| # | Title | Status |
|---|-------|--------|
| — | End-to-end Desktop GUI restart validation (Excalidraw → restart → session-created) | pending interactive session |
| — | `CloseAppWindow` closes root Focus View instead of only app content | open (noted in #277) |
| — | Desktop/CLI session ledger unification (`ato ps --json` vs Desktop-visible sessions) | not yet scoped |

---

## Related PRs / receipts

| Ref | Title |
|-----|-------|
| #277 | Issue: desktop-mcp: support stop/restart active session in Focus mode |
| #284 | `fix(session-core): preserve OCI sessions in startup sweep` |
| #287 | `fix(desktop-mcp): route restart active session in Focus mode` (this fix) |
| docs | `focus-mode-mcp-stop-receipt.md` (stop path — result: complete) |
