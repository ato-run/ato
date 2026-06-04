# AODD Receipt — Installed-App Positive Relaunch (#480 / #481 / #482)

**Date:** 2026-06-04
**Branch:** `verify/480-481-482` = `origin/dev` @ `78a5670b`
(contains PR #489→#480, PR #483→#481, PR #488→#482)

## Outcome

| Issue | Verdict |
|-------|---------|
| #480 deterministic installable fixture + positive relaunch | **PASS** |
| #481 requirements-only Python → actionable failure + `--auto-fix:all` repair | **PASS** |
| #482 invalid `ato://app/<ipk>` → structured `ok:false` | **PASS** |

## Integrated E2E — the validation target

> clean ATO_HOME → install fixture → obtain install_profile_key → open → stop/close
> → reopen via `ato://app/<ipk>` → no consent/review wizard → invalid `ato://app/<bad_ipk>` returns `ok:false`

- **clean ATO_HOME used:** `<repo>/.tmp/ir/h` — populated by the hermetic install path
  (`ato install --from-gh-repo` against a mock Store/GitHub, **no external network**;
  see `crates/ato-cli/tests/installed_relaunch_fixture_install.rs`). `ato install` has no
  local-path mode, so this hermetic install *is* the canonical deterministic-install evidence.
- **fixture:** `crates/ato-cli/tests/fixtures/installed-relaunch-node`
  (`ato-run/installed-relaunch-node` 0.1.0; web runtime, static page on `127.0.0.1:18880`;
  no external services / DB / secrets; not Python-lockfile-dependent).
- **install succeeded; install_profile_key captured:**
  `ipk_82508d85640a941525c49106efdd4071`
  (app `app_824778c6c091d7fd3be1603198b77ba4`, profile `default`,
  revision `rev_fe1fbb91dedb0a2f24e61091e2e2b420`) — see `install-app-record.json`.
- **reopen via `ato://app/<ipk>` succeeded:** MCP `host_dispatch_action {NavigateToUrl, ato://app/<ipk>}`
  → `{"ok":true,"queued_action":"NavigateToUrl"}`; the deno runtime came up and
  `curl http://127.0.0.1:18880/` served **"Ato installed relaunch fixture"** within 1 s.
  (`mcp-valid-relaunch-{request,response}.json`)
- **session record stamped with IPK (the #480 fix, request-scoped typed context):**
  `runs/run-*/session.json` carried `install_profile_key` / `installed_app_id` /
  `install_profile_id` / `install_revision_id` — see `relaunch-session-record.json`.
- **no consent/review wizard on relaunch:** `host_take_screenshot` shows the app
  relaunching directly ("…を起動中…") with no consent/review modal —
  `screenshots/host-relaunch-no-wizard.png`; desktop log:
  `ato_start: opening installed app by ipk (no consent wizard)` (`desktop-log-excerpt.txt`).
- **invalid `ato://app/<bad_ipk>` returned `ok:false`:**
  - unknown key → `{"ok":false,"action":"NavigateToUrl","url":"ato://app/ipk_does_not_exist","reason":"installed_profile_not_found"}` (`mcp-invalid-{request,response}.json`)
  - malformed key → `{"ok":false,...,"reason":"invalid_ato_app_url","detail":"ato://app URL must be shaped as ato://app/<install_profile_key>"}` (`mcp-malformed-response.json`)
  - invalid URLs are intercepted by the `host_dispatch_action` **preflight** at the
    automation-socket layer: no guest launch, no consent wizard, **no handle fallback**
    (no focus-dispatcher launch log line for them).

### Important: stale-binary lesson
The first live attempt reused a pre-built `ato-desktop` (Jun-4 20:02) that **predated PR #488**;
its old code path returned `{ok:true}` and merely logged "no launchable installed profile — ignored"
for a bad ipk. After rebuilding both binaries from `origin/dev` (22:22), the invalid case
correctly returned `{ok:false, reason:installed_profile_not_found}`. Always rebuild the
desktop binaries from the branch under test before a live MCP flow.

## #481 detail (requirements-only Python)

- **Case A (no auto-fix) fails closed, actionably:** `native_python_uv_lock_missing_fail_closed`
  → `ATO_ERR_PROVISIONING_LOCK_INCOMPLETE` / `E104` mentioning `uv.lock`. The error hint
  (`crates/ato-cli/src/adapters/runtime/provisioning/python_lock.rs`) names `--auto-fix:all`,
  `uv pip compile requirements.txt -o uv.lock`, and "commit the generated lockfile upstream".
- **Case B (`--auto-fix:all`) repairs:** `auto_fix_all_generates_requirements_python_lock_and_includes_it`
  runs `uv pip compile … -o uv.lock`, includes it in the pack, and proceeds (no
  `PROVISIONING_LOCK_INCOMPLETE`). (Tests inject a fake `uv`; real `uv` 0.10.0 is installed here.)
- **Case C (uv.lock present) unchanged:** `v03_build_provision_uses_requirements_uv_lock_with_pip_sync`.
- Local requirements-only dev runs keep working via a `uv pip install -r` fallback
  (`cli/commands/run/preflight.rs`); fail-closed applies to build/install (GitHub) provisioning.

## Build / focused tests

See `commands.log`. Baseline (`cargo fmt --all --check`, `cargo check --workspace --all-targets`,
`--bin ato-desktop`, `--bin ato-desktop-mcp`) all PASS. All focused tests for the three issues PASS.

## Known limitations / unrelated findings

- **GUI guest-pane introspection gap (by design):** `browser_snapshot` returns "no WebView pane"
  for the GPUI host surface; relaunch is confirmed via the runtime HTTP probe + host screenshot,
  not a guest a11y tree.
- **`#481` Case-B auto-fix shells out to real `uv`** (mocked in tests); the binary must be installed
  for a live `--auto-fix:all`.
- **Pre-existing, unrelated `cargo test --bin ato-desktop` failures** (NOT in #480/#481/#482 code):
  ~12 failures depend on local environment/preconditions — secret-store identity
  (`secret bridge identity_not_loaded: run \`ato secrets init\``) for `webview::tests::apply_capsule_secrets::*`,
  and local snapshot/serving-root assertions in `settings::tests`, `system_capsule::*`. The source
  files for `settings`/`system_capsule`/`userland` were unchanged in this merge window, and
  `webview.rs`'s secret logic was untouched — so these are pre-existing and environment-dependent.
- **`cargo test -p ato-cli --lib`** (2102 passed with `RUST_MIN_STACK=64M`): all failures are
  pre-existing / environment-dependent and **outside #480/#481/#482**:
  - `lib_tests::app_command_parses_resolve_status_bootstrap_and_repair_forms` overflows the
    default 2 MB test-thread stack in debug (deep clap parsing); passes with a larger stack.
  - `application::runtime_setup::tests::detect_podman_not_found_suggests_install` and the three
    `community::tests::fetch_and_validate_*` pass in isolation — parallel/env pollution
    (local `podman` presence, registry env).
  - `app_control::sample_recipes::tests::catalog_manifests_are_publishable_to_community` fails in
    isolation: bundled sample recipe `openlist-google-drive-crypt` is missing `[source].repository`
    (`sample_recipes.rs`, not touched by these PRs; relates to the separate OpenList recipe work).
- **`capsule-core`:** all pass.
