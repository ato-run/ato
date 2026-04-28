# 📄 Surface Close Semantics

**Document ID:** `SURFACE_CLOSE_SEMANTICS`
**Status:** Draft v0.1 (Phase 2A blocker)
**Target:** ato-desktop v0.5.x
**Last Updated:** 2026-04-29

> **Scope.** UX contract patch for `ato-desktop`. Defines what
> "closing a pane" means, what "stopping a session" means, and how
> they differ. This RFC is a precondition of `SURFACE_MATERIALIZATION`
> Phase 2A — the retention design assumes the contract here is
> already shipped (RFC §1.1 "Phase 2A blockers").
>
> Out of scope: WebView retention internals (covered by
> `SURFACE_MATERIALIZATION.md` §2.1 / §3.3 / §10.3); CLI session
> command surface (already shipped); cross-process daemon mode for
> sessions outliving Desktop (deferred to v1).

## 0. TL;DR

```text
Close is a surface operation.
Stop  is a session operation.
```

`Cmd+W` / pane close hides the surface and **retains** the underlying
capsule session for a bounded TTL. Reopening the same capsule within
the TTL hits the Phase 1 fast path (~167 ms today) — and, after
Phase 2A retention lands, will additionally reattach the already-
hydrated WebView (~50 ms additional saving).

To stop a session, users invoke an **explicit Stop session** action
from the pane context menu or the command palette. App quit and TTL
expiry both stop retained sessions automatically; TTL expiry is
**graceful, best-effort, and non-blocking** so a stuck capsule never
freezes the UI.

## 1. Motivation

`SURFACE_MATERIALIZATION` Phase 1 (PR 4A) measured:

```text
rapid re-click (same pane handle, same process):  ~167 ms
close → re-click within seconds:                  ~5994 ms
```

Both clicks hit identical CLI logic. The asymmetry comes from the
Desktop side: today, `WebViewManager::stop_launched_session` invokes
`ato app session stop` synchronously when a pane closes, deleting the
on-disk session record. The next click finds no record → falls back
to the cold path.

Two consequences fall out of this:

1. **Phase 1 fast path is wasted on the most common reopen pattern.**
   Users typically close and reopen apps — that's exactly when warm
   path matters, and that's exactly when current code throws the
   warm state away.
2. **Phase 2A retention cannot ship.** Retention's preconditions are
   "session record alive + same partition_id + 5 reuse conditions
   pass" (RFC §3.3). All of those depend on the session record
   surviving pane close.

Fixing this is **not** a single-line code change — it's a UX
contract change. "Close" today means "stop"; v0 of this RFC moves
"close" to mean "hide", and introduces a separate, explicit
"stop" action.

## 2. User-visible semantics (v0 contract)

This is the canonical table for v0. Every other section in this RFC
must be consistent with it.

| User action | v0 semantics |
|---|---|
| Pane close (`Cmd+W`, click ✕, drag-out, etc.) | **Hide / detach surface, retain session** for TTL. No process kill, no record deletion. |
| Reopen same capsule **within TTL** | Reattach retained surface if available (post-Phase 2A); otherwise reuse session via Phase 1 fast path. |
| Reopen same capsule **after TTL** | Cold path: spawn via `ato app resolve` + `ato app session start`. |
| **Stop session** (explicit action) | Stop process, delete record, destroy retained surface. Subsequent reopen hits cold path. |
| App quit | Stop all retained sessions, clear retention table. v0 = always stop; daemon mode is v1. |
| TTL expiry | Best-effort graceful stop, delete record, destroy retained surface. Non-blocking. |
| Healthcheck fail on reopen | Discard retained state, fall through to fallback. |

### Glossary (terminology — non-negotiable)

- **Surface**: the visible pane / WebView for a capsule, including
  its document and JS state.
- **Session**: the OS process + bound port + on-disk session record
  that backs a capsule.
- **Close**: a surface operation. Removes the pane from the visible
  layout. Does **not** stop the session.
- **Stop**: a session operation. Terminates the process and removes
  the record. Implicitly destroys any retained surface.
- **Retain**: keep alive in a TTL-bounded table. Applies to sessions
  pre-Phase 2A; applies to surface + session pairs post-Phase 2A
  (§4).

> **Drafting rule.** Throughout this RFC, internal-facing language
> SHOULD use these exact terms. UI strings shown to end users MAY
> deviate (e.g. "Close" → "Close pane", "Stop" → "Quit app"); see §6
> for the user-facing strings.

## 3. Why this is not just "skip the stop call"

A naive fix is "remove the `ato app session stop` call from
`stop_launched_session`." That breaks three invariants:

1. **Resource leak**: closed panes' sessions accumulate forever.
2. **Discoverability**: users have no way to actually stop a
   capsule. They closed the pane and expect it to be gone.
3. **Phase 2A coherence**: retention table must know when to drop a
   `(session_id, partition_id)` pair. Without an explicit "stop" path,
   eviction becomes an arbitrary timeout decision.

Hence the v0 contract introduces three new things together:

- a TTL on retained sessions (§5),
- an explicit **Stop session** UI (§6),
- a deterministic eviction policy (§5, §7).

## 4. State machine

A capsule moves through these states from the Desktop's point of view.

```text
              click capsule
                    │
                    ▼
            ┌───────────────┐
            │  ActiveSurface │  ← visible pane, healthy session
            └───────┬───────┘
                    │
        pane close  │  explicit Stop
        ┌───────────┴────────────┐
        ▼                        ▼
┌─────────────────┐      ┌──────────────────┐
│ RetainedSession │      │     Stopped      │
│   (TTL bound)   │      │ (no process, no  │
│  pre-Phase 2A   │      │     record)      │
└────┬─────┬──────┘      └──────────────────┘
     │     │   (Phase 2A only)
     │     └───────► ┌──────────────────┐
     │               │ RetainedSurface  │
     │               │  (session + WV)  │
     │               └────┬─────────────┘
     │                    │
     │ reopen / TTL / app quit / healthcheck fail / explicit Stop
     │                    │
     ▼                    ▼
   ActiveSurface      ActiveSurface     Stopped
   (Phase 1 fast      (Phase 2A re-     (any of TTL,
    path)              attach)           explicit Stop,
                                         quit)
```

Notes on transitions:

- `ActiveSurface → RetainedSession` is **the headline change** in v0:
  pane close no longer transitions to `Stopped`.
- `RetainedSession → ActiveSurface` is what Phase 1 fast path already
  delivers (~167 ms). No code change required there.
- `RetainedSurface` (the Phase 2A-only state) is a **superset** of
  `RetainedSession` — it adds a hidden WebView. Eviction triggers are
  identical to `RetainedSession`.
- `Stopped` is terminal until the next click rebuilds from cold path.

## 5. Retention TTL and cleanup

### 5.1 Default TTL

```text
v0 default: 5 minutes
```

5 minutes matches `SURFACE_MATERIALIZATION.md` §9.5's retention TTL,
so `RetainedSession` and (later) `RetainedSurface` share one timer.

**v0 constant**: not user-configurable, not capsule-declarable.

**v1**: per-user setting, optionally overrideable per-capsule
declaration. Out of scope for this RFC.

### 5.2 Eviction triggers

Any of these moves a `RetainedSession` (or `RetainedSurface`) to
`Stopped`:

1. **TTL expiry** — wall-clock time since pane close exceeds default
   TTL.
2. **Explicit Stop** — user invokes the stop action (§6).
3. **App quit** — Desktop process exits (any reason).
4. **Healthcheck fail on reopen** — record alive, session_record_validate
   passes the first 4 conditions, but healthcheck times out. Drop
   retention, fall through to cold path. (Already implemented as the
   `healthcheck_failed` outcome in PR 4A.1.)
5. **Memory pressure / WebView crash** (Phase 2A only) — host signals
   resource exhaustion or the retained WebView's renderer crashes.
   Drop the retention slot and fall through.
6. **Max retained reached** — LRU eviction when retention size hits
   the cap (`SURFACE_MATERIALIZATION.md` §9.5: 8 entries v0). Oldest
   non-active retention is stopped first.

### 5.3 Graceful, best-effort, non-blocking stop

TTL expiry, app quit, and LRU eviction are **machine-driven**, not
user-driven. They MUST never block the UI thread or hang on a
misbehaving capsule.

The required pattern:

```text
TTL fires
  ├─ try graceful stop (ato app session stop)
  │   ├─ success                    → delete record, drop retention slot
  │   ├─ timeout (default 2s)       → mark stale, drop retention slot,
  │   │                                log warn, schedule background
  │   │                                cleanup
  │   └─ stop returns error         → log warn, drop retention slot,
  │                                    leave record (next launch will
  │                                    overwrite)
  └─ never block UI; never throw to user
```

Explicit Stop (§6) MAY surface a transient error toast on failure
because the user actively asked. Machine-driven stops MUST stay
silent.

### 5.4 Healthcheck fail discards retention without stop

If a reopen finds a retained session whose healthcheck fails, the
retention table drops the slot **without** invoking
`ato app session stop`. Rationale:

- The session is already unhealthy from the Desktop's perspective.
- A graceful stop call is unlikely to succeed.
- The cold path will overwrite the record on the next launch
  anyway.

This keeps reopen latency bounded by the healthcheck timeout
(`FAST_PATH_HEALTHCHECK_TIMEOUT = 200 ms`), not by the stop timeout
(2 s).

## 6. Explicit Stop UI

v0 ships **two required surfaces** for Stop, plus one provisional
shortcut.

### 6.1 Required: pane context menu / overflow menu

Every pane MUST expose a `Stop session` action via right-click /
overflow menu. Discoverable for new users; visually adjacent to
"Close pane" so users learn the distinction.

UI string (English): `Stop session` (proposal).
UI string (Japanese): `セッションを停止` (proposal).

Both strings are draft and reviewed in the implementation PR.

> **Status (PR 4B.2, 2026-04-29)**: NOT shipped yet. Adding a
> right-click context menu pattern requires net-new GPUI plumbing
> (overlay positioning, dismiss-on-outside-click, hover/select
> interaction); no existing pane in `ato-desktop` ships a context
> menu today. PR 4B.2 satisfies §6.4 via the chrome retention pill +
> command-palette items, which together already give the user a
> visible, single-click way to stop sessions. Pane context menu is
> tracked for a follow-up PR (4B.3) so it can land without blocking
> Phase 2A.

### 6.2 Required: command palette action

The command palette MUST expose two related commands:

- `Stop capsule session` — stops the session associated with the
  currently focused pane (or prompts to pick when no pane is
  focused).
- `Stop all retained sessions` — clears the retention table. Useful
  when a user wants a clean slate without quitting Desktop.

Required because not every user discovers right-click menus, and
power users navigate the palette as their primary surface.

### 6.3 Provisional: keyboard shortcut

A shortcut SHOULD be provided. Initial candidate:

```text
macOS:    Cmd+Shift+W
Windows:  Ctrl+Shift+W
Linux:    Ctrl+Shift+W
```

The exact shortcut is **provisional** — it must be cross-checked
against:

- platform conventions (Cmd+W = close window; Cmd+Shift+W = close all
  windows on macOS in some apps),
- existing GPUI keymap,
- ato-desktop's existing shortcut table.

If a conflict appears, the implementation PR may pick a different
binding. The contract is "a shortcut exists", not "this exact
keystroke".

### 6.4 Discoverability check (v0 hard requirement)

> **v0 MUST provide at least one discoverable way for the user to
> understand that a session is still running after pane close.**

At minimum one of:

- pane-close toast: `Session kept warm for 5 minutes`,
- status indicator: `Running sessions: N`,
- command palette (already required, qualifies if the
  retention-aware item is named visibly: `Stop all retained sessions
  (N running)`).

The implementation PR picks at least one. Adding more is allowed.

This is required because the contract change ("pane close no longer
stops") is invisible by default — without a discoverability hook,
users with hidden-process anxiety cannot tell that retention is
working as designed.

> **Status (PR 4B.2, 2026-04-29)**: §6.4 hard requirement is **met**.
> Three concurrent surfaces ship:
>
> 1. **Chrome retention pill**: `N kept warm` — small clickable pill
>    near the omnibar, only rendered when `retention_count > 0`.
>    Click dispatches `StopAllRetainedSessions`. This is the passive
>    discoverability hook that satisfies "user must be able to tell
>    retention exists without typing".
> 2. **Command palette / omnibar**: `Stop capsule session` (active
>    pane) and `Stop all retained sessions (N)` items appear when
>    the user types `stop` (or implicitly when the bar is empty).
> 3. **Developer log**: `tracing::info!` line on retain (`stderr`).
>
> The activity-panel approach attempted in PR 4B.1 stayed a no-op
> for end users (`state.activity` only renders error-toned entries
> for the launch-failed overlay). The Info push remains in the
> retain path because it costs nothing and may help diagnose a
> subsequent launch failure, but it does NOT count toward §6.4.

## 7. Resource safety

### 7.1 Bounded retention size

```text
v0 cap: 8 entries (matches SURFACE_MATERIALIZATION.md §9.5)
```

LRU eviction kicks in when the cap is exceeded. The oldest
non-active retention is stopped first; "active" means the user is
currently looking at the corresponding pane.

### 7.2 No quit persistence in v0

App quit (clean exit, kill -9, OS shutdown best-effort) stops every
retained session. v0 does not write retention table to disk and does
not advertise daemon mode.

Rationale: persisting retention across Desktop restarts requires:

- a separate background process that owns the sessions,
- IPC between Desktop and that process,
- recovery semantics (which sessions to revive on next start, which
  to drop),
- security review (a hidden background process is a different
  attack surface from a foreground app).

All of that belongs in v1. For v0, the upper bound on a session's
lifetime is the Desktop process's lifetime.

### 7.3 Memory pressure / WebView crash

(Phase 2A relevance.) When the host signals memory pressure or a
retained WebView's renderer crashes, the retention slot is dropped
and the underlying session SHOULD be stopped to free the bound port.
The reopen path falls through to cold.

### 7.4 Healthcheck fail on reopen

Already specified in §5.4. Repeated here under "safety" because it
is the v0 mechanism that prevents a stuck capsule from being
reattached endlessly.

### 7.5 Bounded automatic stop timeout

Machine-driven stops use a `2 s` graceful timeout. After that, the
retention slot is dropped regardless of whether the underlying CLI
stop call has returned. A background task SHOULD continue trying to
stop the orphaned session, but it MUST NOT block any UI-facing path.

## 8. Phase 2A relationship

This RFC is the precondition for `SURFACE_MATERIALIZATION.md`
Phase 2A. The two evolve together:

| Capability | Pre-Phase 2A (this RFC alone) | Post-Phase 2A |
|---|---|---|
| Pane close retains | session + record | session + record + hidden WebView |
| Reopen within TTL | Phase 1 fast path (~167 ms) | retention re-attach (~50 ms more) |
| Reopen after TTL | cold path | cold path |
| TTL eviction stops | session | session + destroys hidden WebView |
| Memory-pressure drop | n/a (no surface retained yet) | drops retained surface; session may also stop |

> **Surface close semantics applies even before WebView retention
> exists.** Before Phase 2A, close retains only the app session.
> After Phase 2A, close retains both the app session and the
> retained surface (the hidden WebView). The user-visible contract
> is identical in both phases — only the underlying speedup grows.

## 9. Implementation outline (informative)

Non-normative pointers for the eventual implementation PR.

- `WebViewManager::stop_launched_session` is the current synchronous
  stop path. The change replaces "stop on pane close" with "demote
  to retention table, schedule TTL eviction".
- The retention table itself is owned by `WebViewManager` (v0:
  session-only) and extended in Phase 2A to also hold the hidden
  WebView.
- The TTL timer is a single shared timer wheel (or async sleep) per
  Desktop process — not per session — to keep the design simple.
- Explicit Stop dispatches through the same `ato app session stop`
  path that exists today; only the trigger changes.
- Healthcheck-fail-on-reopen is already the
  `RecordValidationOutcome::HealthcheckFailed` path from PR 4A.1; no
  new code is needed there.

## 10. Acceptance criteria

### 10.1 v0 (this RFC, pre-Phase 2A)

Status legend: `[x]` shipped, `[~]` shipped with caveat, `[>]`
deferred to follow-up PR (item still required for v0 close).

- [x] Pane close does **not** call `ato app session stop` immediately.
  (PR 4B.1 — `prune_panes` → `retain_launched_session`.)
- [x] Reopen of the same capsule within TTL reuses the same
  `session_id`. Verified in `/tmp/surface-pr4b1.log` (close →
  re-click measured at 160 ms).
- [x] Explicit `Stop session` action removes the record and kills
  the process. (PR 4B.2 — `WebViewManager::stop_active_session`,
  invoked from `Cmd+Shift+W` and the omnibar palette item.)
- [x] After explicit Stop, reopen falls through the fallback / cold
  path. Stop drops retention and deletes the record, so the next
  click finds nothing to fast-path against.
- [x] App quit stops all retained sessions before Desktop exits.
  (PR 4B.1 — `Drop for WebViewManager` drains retention.)
- [x] TTL expiry stops the session, removes the record, drops
  retention. Stop is graceful (`spawn_graceful_stop`), best-effort,
  non-blocking. (PR 4B.1 — `sweep_expired_retention` on every
  `sync_from_state`.)
- [x] Healthcheck-fail on reopen discards retained state and falls
  back. (PR 4A.1, `RecordValidationOutcome::HealthcheckFailed`;
  unchanged here.)
- [x] Retention table is bounded at 8 entries; LRU eviction stops
  the oldest non-active. (PR 4B.1 — `RetentionTable.retain` returns
  `EvictionReason::LruOverflow` for graceful-stop.)
- [x] At least one discoverable indicator tells the user retained
  sessions exist (§6.4). (PR 4B.2 — chrome retention pill +
  command-palette items, both showing the live count.)
- [>] Right-click / overflow menu on every pane offers `Stop
  session`. **Deferred to PR 4B.3** (no existing context-menu
  pattern in ato-desktop; needs new GPUI plumbing). §6.4
  discoverability is already met by the pill + palette so this
  doesn't block Phase 2A.
- [x] Command palette offers `Stop capsule session` and `Stop all
  retained sessions`. (PR 4B.2 — omnibar suggestions on `stop`
  query, with live count for the all-retained item.)
- [~] Keyboard shortcut for Stop is registered. (PR 4B.2 —
  `Cmd+Shift+W` bound to `StopActiveSession` as provisional; if a
  platform/keymap conflict surfaces, rebind in a follow-up.)

### 10.2 Phase 2A additions (informative — owned by `SURFACE_MATERIALIZATION.md` §10.3)

- [ ] Reopen within TTL reattaches retained WebView. No
  `webview_create_*` / `navigation_finished` SURFACE-TIMING stages
  emitted on retention hit (or < 10 ms re-attach).
- [ ] Result kind on the `total` line distinguishes
  `materialized-surface` (retention hit) from
  `materialized-session-fast-path` (Phase 1 only).
- [ ] cross-partition reuse remains structurally impossible.

## 11. Migration path

This RFC is shippable independently of Phase 2A:

- **Step 1**: implement §2 + §5 + §6 + §7 + §10.1. Pane close stops
  calling `ato app session stop`; explicit Stop UI lands; TTL/quit
  cleanup runs. Phase 1 fast path immediately starts hitting on
  close → re-click. No changes to `SURFACE_MATERIALIZATION.md`
  required (the existing fast-path code already handles the case
  where the record is alive).
- **Step 2**: Phase 2A retention lands separately, extending the
  retention table to also hold the hidden WebView. This RFC's table
  in §8 is the contract Phase 2A consumes.

Step 1 is low-risk: every machine-driven stop falls back to the
existing cold path on failure; explicit Stop is a wrapper over the
existing CLI command.

## 12. Open questions

- **Cross-window retention.** When Desktop has multiple windows,
  does TTL count from the moment **any** pane showing the capsule
  closed, or from when the **last** one closed? v0: from the last
  one (consistent with "user is no longer looking"). Document the
  decision in the implementation PR.
- **Pane drag-to-tear-out.** Tearing a pane into a new window should
  not count as close. The implementation PR confirms.
- **Multiple sessions for the same capsule.** Today users can spawn
  multiple sessions of the same handle (different `session_id`s).
  Retention table must key on `session_id`, not on handle, so two
  retained instances of the same capsule are tracked independently.
- **Indicator UX wording.** "Session kept warm for 5 minutes" is a
  draft. Final wording reviewed at implementation time.
- **Telemetry.** SURFACE-TIMING already records fast-path hit rate
  via the absence of `*_subprocess` stages. No new metric required
  for v0; revisit in Phase 2A.

## 13. Related specs

- `SURFACE_MATERIALIZATION.md` — Phase 1 fast path (already shipped),
  Phase 2A retention (depends on this RFC), §1.1 "Phase 1 measured
  result", §9.5 retention size cap.
- `APP_SESSION_MATERIALIZATION.md` — session record schema and
  reuse 5 conditions consumed by `ato-session-core`.
- `ato-desktop/src/orchestrator.rs` — `try_session_record_fast_path`
  (PR 4A.1), `WebViewManager::stop_launched_session` (current
  pane-close path that this RFC modifies).
