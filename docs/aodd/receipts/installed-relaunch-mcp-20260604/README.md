# Installed-app relaunch via MCP — `ato://app/<ipk>` smoke

- **Date:** 2026-06-04
- **Branch:** `test/installed-app-relaunch-mcp-smoke` @ dev `58c0c8a9` (includes merged #471 / #476 / #477)
- **Actor:** Claude operator driving the real `ato-desktop` (GPUI, Focus mode) via the `ato-desktop-mcp` bridge.
- **Goal:** Confirm that the #477 route `host_dispatch_action {action:"NavigateToUrl", url:"ato://app/<ipk>"}` drives the installed-app launch path without re-showing the consent/review wizard, and that `install → open → stop → reopen` holds end-to-end.

## Result

| Phase | Result |
|-------|--------|
| Environment: desktop GUI launches, automation socket published | **PASS** |
| Environment: MCP bridge `initialize` / `tools/list` / live call | **PASS** |
| Negative test: `ato://app/<unknown ipk>` is routed + fail-closed | **PASS** |
| Flow A (first open) / Flow B (reopen) — needs an installed app | **BLOCKED — fixture gap** |

**Overall: partial. The #477 router is verified live and fail-closed; the positive install→reopen path is blocked by the absence of a known-good installable capsule on this machine (no `#471/#476/#477` defect observed).** No smoke "pass" is claimed for the positive flow.

## Environment (PASS)

- Short hermetic root to stay under Unix `SUN_LEN` (104): `ATO_HOME=/tmp/ato.28888c41`, `HOME=/tmp/ato-home.4a206862` (see `environment.txt`).
- Desktop launched with `--skip-onboarding`; `Startup surface opened startup_surface=Start`, Metal/CALayer render logs present → a real window rendered.
- Discovery file published: `desktop-socket.json` → `{"pid":93476,"socket":"/tmp/ato.28888c41/run/ato-desktop-93476.sock"}`.
- MCP `initialize` → `serverInfo ato-desktop-mcp 0.5.5`; `tools/list` = 31 tools incl. all required ones (`mcp-tools-list.txt`); live `auth_status` round-tripped through the socket (`{"signed_in":false,...}`).

## Negative test — fail-closed routing (PASS)

This path needs **no** installed app and exercises the exact #477 chain:
`host_dispatch_action NavigateToUrl` → focus_dispatcher → `app.rs` `NavigateToUrl` → `open_installed_app_by_ipk` → `installed_target_for_app_url` → `None` → warn, no-op.

MCP call (`negative-test-mcp-result.json`):

```json
{"name":"host_dispatch_action","arguments":{"action":"NavigateToUrl","url":"ato://app/ipk_does_not_exist00000000000000"}}
→ {"ok":true,"queued_action":"NavigateToUrl"}
```

Desktop log (`negative-test-desktop-log.txt`):

```
INFO focus dispatcher routes action action=NavigateToUrl
INFO Focus-mode NavigateToUrl url=ato://app/ipk_does_not_exist00000000000000
WARN NavigateToUrl(ato://app): no launchable installed profile — ignored
     error=no installed app matches 'ato://app/ipk_does_not_exist00000000000000' (not installed, or the install is degraded)
```

Confirms:
1. `ato://app/<ipk>` is **routed** (no longer "unsupported scheme — ignored"); the #477 router is live in dev.
2. It reaches `open_installed_app_by_ipk` and fails closed: **no consent wizard, no app window, no handle-based fallback**.
3. `host_take_screenshot` after the call (`negative-test-host-screenshot.png`) shows the host surface with **no consent/review modal** (consistent with fail-closed). Note: no `ato-desktop` window was visible in the captured main display — a separate UI observation (cold-start window visibility / placement), not part of this assertion; the log is the authoritative evidence.

## Flow A / Flow B — BLOCKED by fixture gap

The positive path (`install → ato://app/<ipk> → app opens, no wizard → stop → reopen`) requires a capsule that **installs and provisions cleanly**. No such known-good installable exists on this machine:

- `ato install --from-gh-repo Koh0920/hello-capsule` → fatal `ATO_ERR_PROVISIONING_LOCK_INCOMPLETE: source/python target requires uv.lock for fail-closed provisioning` (`install-attempt-1-plain.stderr.txt`). hello-capsule ships `requirements.txt` but **no `uv.lock`**.
- Retried with `--auto-fix:all` → **same fatal** (`install-attempt-2-autofix-all.stderr.txt`); auto-fix does not synthesize a `uv.lock`.
- `Koh0920/WasedaP2P` is also `source/python` (requirements.txt, no uv.lock) **and** needs a postgres provider + secrets — heavier and not deterministic.

Per the task's Case 4, product code was **not** modified and no positive smoke pass is claimed. The blocker is a missing deterministic installable fixture, not a #471/#476/#477 defect.

## Follow-ups

1. **Deterministic installable fixture** (blocks the positive E2E). Add a minimal capsule that: installs without flaky external services, ships **complete lockfiles** (incl. `uv.lock` for python, or use a node/static driver to avoid uv entirely), starts quickly, exposes a simple HTTP UI, and produces an `install_lifecycle` profile (`install_profile_key`). This unblocks Flow A/B and lets the positive smoke run in CI.
2. **Install UX gap** (separate from #477): a `source/python` capsule with only `requirements.txt` cannot be installed, and `--auto-fix:all` does not generate the required `uv.lock`. Either teach auto-fix to run `ato lock`, or surface the exact `ato lock` remediation in the `install` path. (The canonical example `Koh0920/hello-capsule` currently cannot be installed.)
3. **Invalid-ipk MCP feedback** (already filed against #477): `host_dispatch_action NavigateToUrl ato://app/<bad>` returns `{ok:true, queued_action}` even though the action later fails closed; the MCP caller cannot distinguish an ignored bad ipk from a successful queue. Surface the failure back through the response for AODD.

## Re-run, once a fixture exists

```
host_dispatch_action {action:"NavigateToUrl", url:"ato://app/<ipk>"}
browser_wait_for {selector:"body", timeout:180000}
browser_snapshot ; host_take_screenshot          # expect app window, no wizard
stop_active_session                                # expect {stopped:true, had_active_session:true}
host_dispatch_action {action:"NavigateToUrl", url:"ato://app/<ipk>"}   # reopen, still no wizard
```

Discover the ipk out-of-band (per the `ato-desktop-mcp-flow` skill):

```
grep -rho 'ato://app/ipk_[a-z0-9_]*' "$ATO_HOME/start-history.json" "$ATO_HOME/instances" 2>/dev/null | sort -u
```
