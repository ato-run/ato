# pgweb (v0.17.0) — Tier B

## AODD Receipt

### Test Date
- 2026-05-30 (initial — BLOCKED)
- 2026-05-31 (retest after fix — **complete**)

### Launch Method
Desktop Focus-mode via MCP: `NavigateToUrl capsule://github.com/sosedoff/pgweb`
→ `ForceApprovePending` → WebView pane.

### Result
**complete** — capsule launches end-to-end in the desktop; `guest-capsule`
WebView pane renders the live pgweb UI.

### Evidence (2026-05-31, fixed `ato.exe`)
`browser_tabs` after launch:
```json
{"panes":[{"pane_id":4296967301,"kind":"guest-capsule","window_id":4294967301,
  "url":"http://127.0.0.1:38215/","title":"pgweb",
  "handle":"github.com/sosedoff/pgweb",
  "session_id":"ato-desktop-session-17144","status":"Ready"}]}
```
`browser_take_screenshot` → PNG (`iVBORw0KGgo…`).

Desktop log (full flow, ~8 s, no manual intervention):
```
02:49:01 resolving capsule handle="github.com/sosedoff/pgweb"
02:49:08 capsule session started session_id=ato-desktop-session-17144
02:49:08 upstream HTTP readiness probe passed probe_url=http://127.0.0.1:38215/
02:49:09 FocusGuestPaneRegistry registered app_window_id=4294967301
02:49:09 launch success: AppWindow opened
```

### Root cause (the real #377 blocker)
The desktop launches `ato app session start … --json` via
`std::process::Command::output()`, which blocks until **both** the child's
stdout/stderr pipes reach EOF (all write handles closed) **and** the child
exits. For an OCI/web capsule the orchestrator spawns a long-lived
`<engine> logs --follow <container>` child (oci_provider.rs `logs()`) to mirror
container output. On Windows that child is spawned with `bInheritHandles=TRUE`
and therefore inherits copies of `ato`'s stdout/stderr — i.e. the desktop's
pipe **write** ends. Because `logs --follow` runs until the container stops, it
held those pipe ends open long after `ato app session start` had exited (exit 0,
envelope written), so the desktop's `output()` never observed EOF and the launch
thread hung at "resolving capsule"; the WebView pane was never created. The
parent-death watcher (`app session watch-parent`) is a second such inheriting
child. POSIX was immune because std marks the `output()` pipe fds `CLOEXEC`.

Earlier diagnoses ("source build path", "podman DNS", "Focus dispatcher doesn't
create the pane") were red herrings — the CLI envelope was always produced
correctly; only its delivery to the desktop was blocked.

### Fix
`crates/ato-cli/src/app_control/session.rs` — in `start_session`, when emitting
the JSON envelope (the desktop's mode), clear `HANDLE_FLAG_INHERIT` on this
process's stdout/stderr (Windows-only, via `windows-sys`) **before** the
orchestration spawns any child. No child then captures the desktop's pipes, so
`output()` gets EOF the instant session start exits. The envelope is still
printed through this process's own handles, and service-log mirroring writes to
our stderr at the Rust layer (not via child inheritance), so nothing downstream
needs to inherit these handles.

### Attestations
- [x] CLI preflight PASS (no state bindings, simple OCI container)
- [x] `ato app session start --json` returns full envelope (host_port resolved)
- [x] Desktop `browser_tabs` reports a `guest-capsule` pane bound to the live URL
- [x] Desktop `browser_take_screenshot` returns a PNG of the rendered pgweb UI
- [x] Isolated regression repro (piped `output()`): pipes reach EOF ~0.3 s after
      session-start exits
