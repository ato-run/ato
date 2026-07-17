# desktop: finalize external-browser login (#1077) for promotion toward main

Finalizes **Auth Phase 1c** — Desktop login opens the OS default browser for the
auth_bridge OAuth flow instead of rendering it in an app-embedded Wry WebView
(RFC 8252: native apps must use the system browser). Builds directly on the core
that merged to `nightly` in **#1078**; this branch adds the remaining
platform-specific and credential-safety test coverage and records the
release-sequencing gate so the feature can move `nightly → dev → main` safely.

- **Canonical RFC**: ato-api#261 (Ato Authentication design), Phase 1c.
- **Tracker**: ato-run/ato#1077.
- **Core implementation**: ato-run/ato#1078 (already merged to `nightly`).

## ⚠️ Ships inert — do not cut a desktop release yet

This feature is **fail-closed** and stays inert until a separate, human-gated
step. Two independent, in-code gates enforce that:

1. **Runtime gate.** `EXTERNAL_BROWSER_LOGIN_HARDENING_LANDED`
   (`crates/cli/src/application/auth/store.rs`) is `false`. While false,
   `login_with_store_device_flow_desktop` refuses to open the browser or touch
   the network and instead emits a `desktop_login_failed` event whose
   user-facing message points at `ato login`. It must stay `false` until
   **ato-api#275** (Phase 1b: auth_bridge explicit device confirmation +
   exchange-time device credential) is not just merged but **deployed to
   ato-api production** — merge and deploy are distinct events and this
   depends on both.

2. **Flip-evidence gate (CI-enforced).**
   `hardening_flag_requires_recorded_evidence_when_enabled` (`tests.rs`) fails
   the suite if the flag is ever `true` while
   `EXTERNAL_BROWSER_LOGIN_HARDENING_EVIDENCE` is still the `PENDING` sentinel.
   Flipping the flag therefore *requires* recording real merge+deploy evidence
   in the same commit — it is not a bare boolean a future contributor can flip
   on discipline alone.

3. **Release-publish gate (CI-enforced).** `.github/workflows/desktop-release.yml`'s
   publish job **fails on a release tag** while the flag is still `false`. Since
   #1078 removed the embedded-WebView login, a desktop build cut today would
   have *no* in-app sign-in; this gate makes such a login-less build
   unpublishable. Normal branch CI is unaffected (the gate only runs on release
   tags).

**Go-live sequence (human-gated, later):**
`land ato-api#275 → deploy ato-api to production → flip
EXTERNAL_BROWSER_LOGIN_HARDENING_LANDED to true with recorded evidence in the
same commit → then a desktop release can be cut.`

## Safety properties

- **No browser session token ever reaches the desktop/CLI.** The desktop
  process never reads a browser cookie or session. Its *only* token input is
  `access_token` from the PKCE `/v1/auth/bridge/exchange` response, obtained by
  exchanging a one-time `auth_code` + `code_verifier`. The device credential
  minted server-side (per ato-api#275) is what gets persisted — the browser's
  own session token is never copied across the boundary.
- **No token on stdout or in logs.** The success signal crossing the
  CLI→Dock boundary (`desktop_login_completed`) carries only the publisher
  handle and a storage-location label. Failure diagnostics from ato-api are
  split by `sanitize_bridge_failure` into a generic user-facing `message` and a
  logs-only `detail`; the Dock forwards only `message` into toasts.
- **Fail-closed by default** (see gates above).
- **URL passed as a discrete argument on every OS** — the login URL carries a
  server-chosen `next` parameter and is handed to the browser as one argument,
  never spliced into a shell string.

## What this branch adds on top of #1078

- **Per-OS browser-open argv is now pure and unit-tested.**
  `try_open_browser` (`crates/cli/src/application/auth/prompt.rs`) was refactored
  to build its command via a pure `browser_open_command(os, url)` that takes the
  OS explicitly, so the argv for macOS / Linux / Windows is testable from a
  single host build (behavior unchanged: macOS `open <url>`, Linux
  `xdg-open <url>`, Windows `cmd /C start "" <url>`).
- **Completion-event token invariant is now pinned by a test.** The
  `desktop_login_completed` NDJSON payload is built by a pure
  `desktop_login_completed_event`, guarded by a test asserting it can never
  carry a token field.

## Test coverage

Rust workspace only (no JS tooling). Pure, gate-independent unit tests — the
runtime gate keeps the full desktop poll loop unreachable in tests, so the
logic is covered by extracting and directly testing each pure piece rather than
by flipping the gate or mocking the whole flow:

**OS-specific (macOS / Windows / Linux), `crates/cli/src/application/auth/tests.rs`:**
- `browser_open_command_macos_uses_open_with_url_as_a_single_arg`
- `browser_open_command_linux_uses_xdg_open_with_url_as_a_single_arg`
- `browser_open_command_windows_uses_cmd_start_with_empty_title_and_url_as_a_single_arg`
- `browser_open_command_keeps_a_url_with_shell_metacharacters_as_one_argument`
  (all three OSes; proves the `next`-carrying URL is never shell-spliced)

**Credential / token safety:**
- `desktop_login_completed_event_never_carries_a_token` (+ `_has_expected_shape`,
  `_allows_absent_handle`)
- `persist_session_token_headless_uses_canonical_file_with_0600`,
  `persist_session_token_interactive_falls_back_to_memory_without_identity`,
  `persist_session_token_interactive_writes_to_age_when_identity_loaded`
  (credential save)

**Bridge flow / gate (already present from #1078):**
- `desktop_device_flow_fails_closed_pending_bridge_hardening` (fail-closed)
- `hardening_flag_requires_recorded_evidence_when_enabled` (CI flip gate)
- `desktop_login_gate_user_message_has_no_internal_jargon`,
  `..._does_not_imply_terminal_fallback_is_safe`
- `compute_poll_timing_*` (poll timeout cap / interval floor — the expiry
  arithmetic)
- `browser_launch_failed_event_carries_login_url_and_sanitized_message`
  (browser-open failure → actionable fallback payload)
- `sanitize_bridge_failure_keeps_raw_detail_out_of_the_user_message`
  (poll / exchange / init failure sanitization)

**Dock side, `crates/desktop/src/system_capsule/ato_dock/mod.rs`:**
- `classify_ndjson_line` cases: completed (poll success), failed (timeout /
  rejected), `detail`-not-forwarded (exchange failure), browser-launch-failed
  forwards `login_url` as its own field, malformed/unrecognized ignored
- `login_guard_rejects_concurrent_acquire_until_released` (single-flight)

## Rollout / rollback

- Ships inert (flag `false`); no user-visible behavior change until the
  human-gated flip described above.
- Rollback: revert this branch's commits (the #1078 core reverts independently).

## Human verification required before the flag is flipped

Real interactive browser-opening cannot be automated in a sandbox. Before
flipping the flag, a human must, on a real machine: trigger Desktop login,
confirm the OS default browser opens (not an embedded window) at the auth_bridge
activation URL, and confirm the desktop app keeps polling and completes login
exactly as `ato login` does. Attach a short screen recording to the flip PR.
