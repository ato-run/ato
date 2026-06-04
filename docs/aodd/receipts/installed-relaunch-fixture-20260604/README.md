# Installed Relaunch Fixture Receipt

Date: 2026-06-04

Scope: issue #480, deterministic installable fixture for installed-app relaunch. This receipt covers the committed fixture, hermetic mock install, installed launch session stamping, and live Desktop/MCP relaunch smoke.

## Result

| Check | Result | Evidence |
| --- | --- | --- |
| Deterministic fixture exists | PASS | `crates/ato-cli/tests/fixtures/installed-relaunch-node/` |
| Manifest uses v0.3 target shape | PASS | `[targets.main]` with `runtime = "source"`, `driver = "node"`, `run = "node server.js"`, `port = 18880` |
| No external Store/GitHub network in install test | PASS | mock server saw only `install-draft` and deterministic tarball paths |
| Install emits stable IPK | PASS | `ipk_82508d85640a941525c49106efdd4071` |
| Revision/materialized artifact exists | PASS | current revision `rev_fe1fbb91dedb0a2f24e61091e2e2b420` |
| Desktop MCP `NavigateToUrl ato://app/<ipk>` queues | PASS | `{ok:true, queued_action:"NavigateToUrl"}` |
| Desktop relaunch starts fixture runtime | PASS | `http://127.0.0.1:18880/` served `Ato installed relaunch fixture` after MCP dispatch |
| Installed session record carries IPK | PASS | session record includes `install_profile_key`, `install_revision_id`, and `installed_app_id` |
| Consent/review wizard on relaunch | PASS | host screenshot showed no consent/review wizard; installed path used `ato launch <ipk>` |
| MCP guest `browser_snapshot` | PARTIAL | current live smoke still returns `no WebView pane`; runtime and session record are valid |

## Notes

- The fixture is intentionally dependency-free except for Node itself; `package.json` declares `type = "commonjs"` so `server.js` runs deterministically.
- The GitHub tarball shape used by the test has a fixed root prefix, fixed SHA, sorted entries, fixed mtime, uid/gid, and mode.
- `port = 18880` is fixed by the fixture; the integration test is serial and checks port availability before running.
- The Desktop smoke originally exposed a real relaunch blocker: provisional session records written during dependency materialization lacked install lifecycle fields. This change stamps lifecycle fields from the scoped `ato launch` context so Desktop can find the installed session by IPK.
- #481 and #482 were not implemented.

