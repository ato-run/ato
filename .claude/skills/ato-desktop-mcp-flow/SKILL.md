---
name: ato-desktop-mcp-flow
description: "Drive the running ato-desktop (GPUI/Focus mode) end-to-end through the ato-desktop-mcp bridge to exercise real user flows: open a capsule, satisfy config/consent/secret gates, reopen an installed app via ato://app/<ipk>, stop/restart sessions, and inspect both guest WebViews and the GPUI host surface. Use for AODD-style operation, not unit tests."
argument-hint: "Describe the flow to drive, e.g. launch WasedaP2P end-to-end, reopen an installed app without re-consent, or stop the active session via MCP"
user-invocable: true
---

# Ato Desktop MCP Flow Driving

## What This Skill Does

This skill is the operating playbook for **driving the real ato-desktop UI
through the `ato-desktop-mcp` bridge** — opening apps, clearing launch gates,
and inspecting the result the way an autonomous operator (AODD) would.

It is the *flow* companion to `ato-cli-desktop-testing`:

| Skill | Scope |
|-------|-------|
| `ato-cli-desktop-testing` | hermetic env (`ATO_HOME`) setup, focused cargo tests, **transport-level** MCP smoke (`initialize` / `tools/list`), socket discovery |
| `ato-desktop-mcp-flow` (this) | the **tool sequences** that move a live desktop through actual user flows + how to inspect and where the surface stops |

Start a hermetic run with `ato-cli-desktop-testing`, then use this skill to
operate the desktop inside that same `ATO_HOME`.

## When To Use

- run / reproduce a full launch flow against a live desktop via MCP
- verify a fix end-to-end where "does the real UI advance?" is the gate
- produce an AODD Usecase Receipt (see `.claude/skills/aodd/SKILL.md`)
- check that an installed app reopens without re-showing consent

Do **not** use this skill for pure unit/integration tests (use cargo) or for a
transport-only sanity check (use `ato-cli-desktop-testing`).

## Prerequisites

1. **Build both binaries** (the `ato-desktop` crate has its own `target/`):

   ```bash
   cargo build --manifest-path crates/ato-desktop/Cargo.toml --bin ato-desktop
   cargo build --manifest-path crates/ato-desktop/Cargo.toml --bin ato-desktop-mcp
   # → crates/ato-desktop/target/debug/ato-desktop{,-mcp}
   ```

2. **Shared `ATO_HOME`.** The desktop publishes its socket to
   `${ATO_HOME}/run/ato-desktop-current.json`; the MCP discovers it from the
   same path. Launch both inside one hermetic shell (see
   `ato-cli-desktop-testing` → "Hermetic desktop + MCP smoke flow").

3. **macOS Accessibility** (only for `host_press_key` / `host_activate_app`).
   Synthetic keystrokes via osascript need an Accessibility grant for the
   process running osascript. If you cannot grant it, **drive host gestures
   through `host_dispatch_action` instead** — it routes through the automation
   socket and needs no Accessibility permission.

4. **The desktop runs in Focus mode** (the `focus_dispatcher` owns the
   automation socket). This changes which tools do what — see below.

## Mental Model: Focus mode tool surface

The desktop is GPUI **host chrome** + embedded **guest WebViews**. The MCP
tools split along that line. Getting this wrong is the #1 reason a flow stalls.

| Layer | Reach it with | Notes |
|-------|---------------|-------|
| **App-level navigation** (open a capsule / app / URL) | `host_dispatch_action {action:"NavigateToUrl", url}` | In Focus mode `browser_navigate` (OpenUrl) only works against an already-open **dock** pane; for opening *new* things use the host action. |
| **Guest WebView page** (the app's own DOM) | `browser_snapshot`, `browser_click`, `browser_fill`, `browser_wait_for`, `browser_evaluate`, … | `pane_id` defaults to `0` = frontmost guest pane (falls back to the dock). |
| **Launch gates** (config / consent) | `set_capsule_secrets`, `approve_execution_plan_consent` | These are the modal-equivalent surfaces; they re-arm the launch. |
| **Session lifecycle** | `stop_active_session`, `restart_active_session` | Argument-less; target the active pane. |
| **GPUI host surfaces** (Start / Store / Settings windows, boot wizard, chrome) | `host_take_screenshot`, plus `host_dispatch_action` for known actions | **Not reachable by `browser_*`** — they are system-capsule WebViews / native chrome, not the active guest pane. `browser_snapshot` will not see them. |
| **Sign-in state** | `auth_status` | Never returns the token. |

`host_dispatch_action` is the AODD bypass for host gestures; query the valid
action list from the tool's error message (it echoes `KNOWN_ACTIONS`) or
`tools/list`. Common ones: `OpenStartWindow`, `OpenStoreWindow`,
`OpenCardSwitcher`, `ShowSettings`, `NavigateToUrl`, `ForceApprovePending`,
`SkipOnboarding`, `CompleteOnboarding`.

## Canonical Flows

### A. Launch a fresh capsule end-to-end

The gates appear on the guest **browser surface** as status text; poll it
rather than guessing timeouts.

1. `host_dispatch_action {action:"NavigateToUrl", url:"capsule://github.com/Koh0920/WasedaP2P"}`
2. `browser_snapshot` → expect `awaiting pre-launch requirements: config:app` (or `resolving`)
3. `set_capsule_secrets {handle:"github.com/Koh0920/WasedaP2P", secrets:{SECRET_KEY:"…", PG_PASSWORD:"…"}}`
4. `browser_snapshot` → expect `awaiting pre-launch requirements: consent:app[, consent:web]`
5. `approve_execution_plan_consent {handle:"capsule://github.com/Koh0920/WasedaP2P"}`
   — **repeat once per target** if the snapshot lists multiple `consent:*`.
6. `browser_wait_for {selector:"body", timeout:180000}` — cold start can be
   minutes; the MCP extends its read timeout for long `wait_for`.
7. `browser_snapshot` → a live a11y tree for the loaded guest page = success.

Notes:
- `set_capsule_secrets` returns an MCP **error** on disk-write failure (it does
  not silently swallow) — treat `isError:true` as a real stop.
- Submitting secrets *before* the config gate surfaces persists them but does
  not retroactively re-arm an in-flight launch; submit after step 2 shows the
  gate.

### B. Reopen an installed app via `ato://app/<ipk>` (no re-consent)

This is the install-profile relaunch path (#261). An app installed once must
reopen through its **durable identity** `ato://app/<install_profile_key>` and
must **not** re-show the consent wizard.

1. **Find the ipk** (there is no MCP tool to enumerate installs — read it
   out-of-band from the hermetic `ATO_HOME`):

   ```bash
   # app_url is persisted in Start history (start-history.json) and derivable
   # from the install store under instances/.
   grep -rho 'ato://app/ipk_[a-z0-9_]*' \
     "${ATO_HOME}/start-history.json" "${ATO_HOME}/instances" 2>/dev/null | sort -u
   ```

2. `host_dispatch_action {action:"NavigateToUrl", url:"ato://app/ipk_…"}`
3. `browser_wait_for {selector:"body", timeout:120000}` then `browser_snapshot`.

Pass criteria:
- the app window opens **without** a consent/review modal appearing
  (`host_take_screenshot` to confirm no wizard);
- a live session is produced (verify with `ato app session list` /
  `stop_active_session` returning `had_active_session:true`).

Fail-closed: a wrong/degraded ipk is **ignored** (logged
`NavigateToUrl(ato://app): no launchable installed profile`) rather than
degrading to a handle launch — the snapshot will not advance. That is the
intended behavior, not a bug.

### C. Stop / restart the active session

- `stop_active_session` → `{ok, stopped, had_active_session, session_id, handle}`.
  `stopped:false, had_active_session:false` = nothing was running (idempotent).
  `stopped:false, had_active_session:true` = the underlying stop failed —
  inspect ports/processes (`cleanup_host_resources {dry_run:true}`).
- `restart_active_session` → stops and reopens the same route (CapsuleHandle /
  CapsuleUrl only); the root window stays open.

### D. Inspect the GPUI host surface

`browser_*` cannot see host chrome or system-capsule windows. Use:

- `host_take_screenshot {}` → writes a PNG under `${ATO_HOME}/aodd/`, returns
  `{ok, path}`. Read the file to see the real screen state.
- `host_activate_app {process_name:"ato-desktop"}` then `host_press_key` for
  native keystrokes — only if Accessibility is granted; otherwise prefer
  `host_dispatch_action`.

## Waiting & inspection patterns

- Prefer `browser_wait_for` over fixed sleeps; it respects the caller `timeout`
  (ms) and the MCP scales its read budget to outlive it.
- `browser_snapshot` returns the page status string while a launch is still
  resolving (`page not yet loaded (session: resolving; launch still in
  progress)`) — that is information, poll it; it is not an error.
- `browser_console_messages` drains and clears the guest console buffer.

## Known limitations (current surface gaps)

Document these in any receipt; do not report a flow "blocked" when it is one of
these by-design boundaries:

1. **System-capsule windows (Start / Store / Settings) are not driveable by
   `browser_*`.** Their tiles/toggles are HTML in their own WebView, invisible
   to the active-guest-pane `browser_snapshot`. Drive them via
   `host_dispatch_action` actions where one exists; otherwise the gesture is
   not yet automatable.
2. **No MCP tool enumerates installed apps / ipks.** Discover the ipk from the
   hermetic `ATO_HOME` (flow B step 1).
3. **Focus-mode `browser_navigate` (OpenUrl)** only targets the dock pane; use
   `host_dispatch_action {action:"NavigateToUrl"}` to open new capsules/apps.
4. **`host_press_key` needs macOS Accessibility.** Use `host_dispatch_action`
   to avoid the permission entirely.

## Verification checklist

- [ ] both binaries built from the branch under test
- [ ] desktop + MCP share the run's `ATO_HOME` (`run/ato-desktop-current.json` present)
- [ ] each flow step driven through a real MCP tool (no manual desktop clicks)
- [ ] launch gates cleared via `set_capsule_secrets` / `approve_execution_plan_consent`
- [ ] installed-app reopen confirmed **without** a consent modal (flow B)
- [ ] final state inspected (`browser_snapshot` for guest, `host_take_screenshot` for host)

## Reporting

Report concisely: which flow, the exact tool sequence run, where it advanced or
stalled, whether any stall was a by-design gap above or a real defect, and the
evidence (snapshot text, screenshot path under `${ATO_HOME}/aodd/`, session
ids). For a frictions-into-development-input writeup, hand off to the `aodd`
skill.
