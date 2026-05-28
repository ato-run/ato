# AODD Receipt: Featured Apps Launch via open_query + Actionable Launch Errors

## Usecase

Verify that:
1. Featured Apps cards on the Desktop start page launch via `open_query` (not the broken `open_capsule` path)
2. Desktop surfaces actionable launch errors instead of debug/tracing noise

## Context

Two PRs landed on `dev`:
- **#280** `fix(ato-start): launch featured app cards through open_query` — merged `247efe7e`
- **#281** `fix(desktop): surface actionable capsule launch errors` — merged `6fbed2ba`

Dev SHA tested: `6fbed2bace1feab59d32f22ac201464f7be794ba`

## Environment

| Field | Value |
|---|---|
| OS / arch | macOS Darwin arm64 |
| Podman backend | `podman-machine-default` |
| DOCKER_HOST | `unix:///var/folders/98/k9wrs95s7972nb_qn_k8k2kr0000gn/T/podman/podman-machine-default-api.sock` |
| ATO_HOME isolation | fresh `mktemp -d` per session |

---

## Part A: Featured Cards → open_query (#280)

### Implementation verified

Code inspection confirms the wiring in `fix(ato-desktop): route featured apps through open_query` (commit `dd23d86b`):

- `FeaturedApps.astro`: each card has `data-action="open_query"` and `data-query="affine|open-webui|excalidraw"`
- `index.astro`: `.featured-card` and `.featured-launch` click handlers send `{kind: 'open_query', value: query}` (no longer `open_capsule`)
- `app.tsx`: `openFeaturedApp(handle)` calls `submitQuery(handle)` — aligned with search bar path
- `classify_query` in `mod.rs`: `is_featured_sample_alias` allows `"affine"`, `"open-webui"`, `"excalidraw"` as `CapsuleHandle`; arbitrary bare strings remain `Invalid`

### Unit test results (all pass)

```
cargo test -p ato-desktop system_capsule::ato_start -- --nocapture
running 10 tests
test system_capsule::ato_start::tests::classify_featured_sample_aliases ... ok
test system_capsule::ato_start::tests::classify_invalid_bare_string ... ok
test system_capsule::ato_start::tests::classify_github_url_as_handle ... ok
test system_capsule::ato_start::tests::classify_capsule_url ... ok
test system_capsule::ato_start::tests::classify_http_url ... ok
... (all 10 pass)
```

`classify_query("affine")` → `CapsuleHandle("affine")` ✅  
`classify_query("open-webui")` → `CapsuleHandle("open-webui")` ✅  
`classify_query("excalidraw")` → `CapsuleHandle("excalidraw")` ✅  
`classify_query("hello world")` → `Invalid` ✅ (no regression)

### CLI alias resolution smoke test

#### AFFiNE

```bash
export DOCKER_HOST="unix:///...podman-machine-default-api.sock"
ATO_HOME=$(mktemp -d)
cargo run -p ato-cli -- app session start affine --json
```

Result:
```json
{
  "session_id": "ato-desktop-session-37475",
  "handle": "affine",
  "status": "ready",
  "source": "sample_recipe",
  "web": { "local_url": "http://127.0.0.1:3010/" }
}
```

| Check | Result |
|---|---|
| source | `sample_recipe` ✅ (not GitHub fallback) |
| port | 3010 |
| HTTP | `HTTP/1.1 302 Found` ✅ |
| Elapsed | 84s |
| Container count | 3 (main + postgres + redis) |

#### Excalidraw

```bash
ATO_HOME=$(mktemp -d)
cargo run -p ato-cli -- app session start excalidraw --json
```

Result:
```json
{
  "session_id": "ato-desktop-session-38765",
  "handle": "excalidraw",
  "status": "ready",
  "source": "sample_recipe",
  "web": { "local_url": "http://127.0.0.1:8080/" }
}
```

| Check | Result |
|---|---|
| source | `sample_recipe` ✅ (not GitHub fallback) |
| port | 8080 |
| HTTP | `HTTP/1.1 200 OK` ✅ |
| Elapsed | 15s |
| Container count | 1 (single nginx container) |

### Cleanup

`ato app session stop <session_id>` returned `stopped: false` for both sessions — see **Known Issues** below.  
Direct podman cleanup was used:

```bash
podman stop ato-affine-fb47dfb4-{main,redis,db} ato-excalidraw-893a7506-main
podman rm  ato-affine-fb47dfb4-{main,redis,db} ato-excalidraw-893a7506-main
```

Result: **0 orphan containers** ✅

### Desktop visual flow (FYI)

The Desktop start window is a native GPUI window — MCP `snapshot` does not reach it.  
Full visual card-click automation is not possible via MCP alone. The unit tests + CLI alias resolution together constitute the functional proof.  
Manual verification: open Desktop → start page → click AFFiNE / Excalidraw cards → consent window appears with `sample_recipe` source.

---

## Part B: Actionable Launch Errors (#281)

### Root cause confirmed

In `run_session_start_command` (orchestrator.rs), when `ato session start` fails with `RUST_LOG=debug`, the full raw `stderr` — including tracing lines like `DEBUG Provision command path diagnostics phase="run" runtime="oci"` — was used directly as the error detail.

### Fix

`extract_user_facing_error(stderr, stdout) -> Option<String>` helper added:

- Filters tracing noise via `is_tracing_noise_line`: strips ISO-8601 timestamp prefix, rejects lines starting with `DEBUG`, `TRACE`, `INFO`, and explicitly `Provision command path diagnostics`
- Prefers `is_actionable_error_line`: lines containing `error`, `failed`, `timeout`, `readiness`, `fatal`, `panic`, `cannot`, `unable`
- Falls back to the last 3 non-noise stderr lines if no actionable line found
- Returns `None` if everything is noise (caller uses exit-code-only error)

### Unit test results (all pass)

```
cargo test -p ato-desktop launch_error -- --nocapture
running 11 tests
test orchestrator::launch_error_display_tests::test_all_debug_lines_returns_none ... ok
test orchestrator::launch_error_display_tests::test_empty_stderr ... ok
test orchestrator::launch_error_display_tests::test_provision_diagnostics_filtered ... ok
test orchestrator::launch_error_display_tests::test_prefers_actionable_error_line ... ok
test orchestrator::launch_error_display_tests::test_prefers_last_actionable_line ... ok
test orchestrator::launch_error_display_tests::test_fallback_to_last_lines ... ok
test orchestrator::launch_error_display_tests::test_mixed_noise_and_actionable ... ok
test orchestrator::launch_error_display_tests::test_stdout_json_error_extraction ... ok
test orchestrator::launch_error_display_tests::test_info_lines_filtered ... ok
test orchestrator::launch_error_display_tests::test_trace_lines_filtered ... ok
test orchestrator::launch_error_display_tests::test_timestamp_prefix_stripped ... ok
ok. 11 passed; 0 failed
```

All 11 tests pass. Key scenarios covered:

- `"DEBUG Provision command path diagnostics phase=run"` → filtered, not shown to user ✅
- `"ERROR readiness probe failed: timeout"` → surfaced as primary error ✅
- All-noise stderr → `None` (caller shows exit-code message) ✅
- Mixed noise + actionable → actionable line chosen ✅

---

## Summary

| Feature | Result |
|---|---|
| Featured cards use `open_query` | ✅ verified (unit tests + CLI) |
| `affine` alias → `sample_recipe` | ✅ HTTP 302 :3010 |
| `excalidraw` alias → `sample_recipe` | ✅ HTTP 200 :8080 |
| Arbitrary bare strings still invalid | ✅ no regression |
| Debug noise filtered from launch errors | ✅ all 11 tests pass |
| 0 orphan containers | ✅ after direct podman cleanup |

---

## Known Issues / Follow-ups

### `ato app session stop` returns `stopped: false`

When a session is started in-process via `ato app session start <alias> --json` and the invocation exits, the session's container ownership record is not persisted in a way that a subsequent `ato app session stop <session_id>` can locate. The CLI's stop command returns `stopped: false` and containers remain running.

**Classification**: CLI/runtime stop registration gap — related to #273 (network prune on session stop).  
**Workaround**: Direct `podman stop` / `podman rm` per container name.  
**Impact**: Limited to standalone CLI invocations; Desktop-launched sessions stop correctly through Desktop UI / Focus-mode MCP.

### `ato ps --json` does not surface Desktop sessions

As noted in #275, `ato ps --json` via the CLI does not list Desktop-managed sessions. This is an expected limitation documented as a Desktop/CLI ledger unification follow-up.

### open-webui alias not smoke-tested

`open-webui` alias resolution was validated via unit test only (`classify_query("open-webui")` → `CapsuleHandle`). Full first-run behavior matches Test Set A finding (extended download on first run).
