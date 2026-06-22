# PR Summary: OCI Provider Model (PR 7 – PR 11.5)

**Branch:** `feat/oci-provider-model`  
**Status:** Ready for review

---

## Overview

This branch introduces a complete OCI container execution path into Ato CLI —
from manifest declaration through lock persistence, service orchestration, and
three distinct import entry points — without touching the existing Wasm/native
execution paths.

The official runtime provider is **Podman** (rootless). The legacy Bollard
(`OciRuntimeClient`) path is quarantined behind a feature flag and is not used
in any new code added in this branch.

---

## Architecture

```
Capsule manifest (runtime = "oci" + service graph)
        │
        ▼
OciProvider trait (probe / resolve_image / pull / create / start / stop / logs)
        │
        ├─── PodmanProvider   ← official v1 (rootless Podman socket)
        └─── FakeOciProvider  ← test doubles for all unit/integration tests

Entry points:
  1. Explicit OCI graph (capsule.toml with [targets.*.runtime = "oci"])
  2. --oci-compose           (Docker Compose subset importer)
  3. --oci-install-sh        (docker run intent extractor)

All three converge on:
  OciComposeLock (ato.oci.lock.json) → OciOrchestrationPlan → oci_multi_service executor
```

---

## Slices

| PR | Description | Key files |
|----|-------------|-----------|
| PR 7 | Single-target Podman execution: pull → create → start → logs → stop → remove | `oci_single_target.rs`, `oci_provider.rs` |
| PR 8 | Multi-service execution: service DAG, internal network, state bindings, cleanup | `oci_multi_service.rs` |
| PR 9 | Docker Compose subset importer (services, volumes, depends_on, networks) | `compose.rs` |
| PR 10 | `--oci-compose` CLI flag: parse → resolve → lock → execute | `oci_compose_runner.rs`, `run.rs` |
| PR 10.5 | Failure-path hardening: failure tests, diagnostics, opt-in real Podman smoke | `oci_compose_runner.rs` (tests) |
| PR 10.6 | OCI compose lock persistence: `ato.oci.lock.json`, replay on rerun | `oci_compose_lock.rs` |
| PR 11 | install.sh / docker run intent extractor: pure static parser, no shell exec | `docker_run_script.rs`, `install_sh_runner.rs` |
| PR 11.5 | Blinko smoke, invariant hardening, merge readiness | tests, RFC updates, diagnostics |

---

## Supported entry points

### 1. Explicit OCI graph (`capsule.toml`)

```toml
[targets.app]
runtime = "oci"
image = "blinkospace/blinko:latest"
port = 1111

[targets.db]
runtime = "oci"
image = "postgres:14"

[services.main]
target = "app"
depends_on = ["database"]

[services.database]
target = "db"

[[services.database.state_bindings]]
state = "pg_data"
target = "/var/lib/postgresql/data"
```

### 2. `--oci-compose` (experimental, hidden flag)

```sh
ato run . --oci-compose
```

Discovers `docker-compose.yml` in the project directory, resolves image
digests, writes/updates `ato.oci.lock.json`, and executes all services
through the multi-service Podman path.

### 3. `--oci-install-sh` (experimental, hidden flag)

```sh
ato run . --oci-install-sh
```

Discovers `install.sh` / `setup.sh` / `start.sh` / `run.sh`, extracts
`docker run` intent statically (no shell execution), and executes the same
path as `--oci-compose`.

---

## Not supported (intentional scope limits)

| Feature | Reason |
|---------|--------|
| Arbitrary shell execution in install.sh | Security boundary — script is parsed, not executed |
| Full Docker Compose compatibility | Only a safe subset; complex features deferred |
| `--privileged` containers | Hard security reject |
| `--network host` | Hard security reject |
| Absolute host bind mounts | Hard security reject |
| Auto-detection in normal `ato run .` | OCI paths remain opt-in experimental |
| CDP / Chrome DevTools Protocol | Unrelated; macOS/Linux incompatible |
| Multiple import formats in one project | Compose + install.sh mixed not tested |
| `docker build` within install scripts | Not extracted; would require image build support |

---

## OCI lock file (`ato.oci.lock.json`)

The OCI lock is an experimental sidecar file, separate from `ato.lock.json`.

Fields: resolved image digest, platform, provider semantics label, import
source hash.

Never stored: container id, network id, volume id, host port, secret values.

Future: fields will migrate into `ato.lock.json` once the OCI model is stable.
See `OCI_PROVIDER_LOCK_IDENTITY_SPEC.md §15.2` for the migration plan.

---

## Test matrix

| Suite | Filter | Count | Notes |
|-------|--------|-------|-------|
| `capsule` | `docker_run_script` | 18 | Pure importer unit tests |
| `capsule` | `oci_compose_lock` | 18+ | Lock persistence tests |
| `capsule` | `compose_import` | 20+ | Compose importer tests |
| `capsule` | `derive` | 32 | ExecutionPlan identity tests |
| `ato-cli` | `oci_single_target` | 14 | Single container lifecycle |
| `ato-cli` | `oci_multi_service` | 18 | Service DAG execution |
| `ato-cli` | `oci_compose` | 23 | Compose runner + lock |
| `ato-cli` | `install_sh` | 12 | install.sh runner |
| `ato-cli` | `oci_provider` | 20 | Provider readiness / selector |
| `ato-cli` | `podman_probe` | 6 | Podman probe typed errors |

All tests pass. Real Podman tests are `#[ignore]` unless `ATO_TEST_REAL_PODMAN=1`.

---

## Known blockers (pre-existing, not regressions)

1. `cargo test --workspace` triggers an interactive consent prompt in the
   `cli::commands::run::preflight` test. Always test with `-p ato-cli` or
   `-p capsule` with module filters.

2. `install_aliases_in_same_scope_are_rejected` test in
   `cli::commands::run::preflight::tests` fails pre-existing (unrelated to OCI).

---

## Running the validation suite

```sh
# Format
cargo fmt --all

# Type-check
cargo check -p capsule -p ato-cli

# Unit tests
cargo test -p capsule docker_run_script --lib
cargo test -p capsule compose_import --lib
cargo test -p capsule oci_compose_lock --lib
cargo test -p capsule derive --lib
cargo test -p ato-cli install_sh --lib
cargo test -p ato-cli oci_compose --lib
cargo test -p ato-cli oci_multi_service --lib
cargo test -p ato-cli oci_single_target --lib
cargo test -p ato-cli oci_provider --lib
cargo test -p ato-cli podman_probe --lib

# Optional: real Podman smoke (requires Podman + machine on macOS)
ATO_TEST_REAL_PODMAN=1 cargo test -p ato-cli real_podman -- --ignored --nocapture
```

---

## PR 11.6 — Real Blinko AODD: passed

Validated 2026-05-22, branch `feat/oci-provider-model`.
Host: macOS arm64, Podman 5.4.0 (podman-machine-default, applehv).
Source: `git clone https://github.com/blinkospace/blinko /tmp/blinko-aodd`.

Receipts: `.tmp/aodd-receipts/`

### Scenario A — install.sh import path: complete ✅

Two blockers found and fixed before re-run:

| Blocker | Cause | Fix |
|---------|-------|-----|
| `blinko-website → image: $volume_mount` | Shell variable token in flag position captured as image ref | Skip `$variable` tokens in `parse_docker_run` when more tokens follow |
| `postgres:14` fails E999 "ambiguous: 16 platform(s)" | Multi-arch manifest with no `requested_platform` | `auto_select_platform()` maps host arch to OCI arch; falls back to `linux/amd64` |

After fixes: both digests resolved (`linux/arm64`), app started, `http://127.0.0.1:37079/` returns HTTP 200.

### Scenario B — lock replay: complete ✅

Second run shows `♻️ Reusing lock` for both services. Source hash stable. New session suffix per run.

### Scenario C — cleanup: degraded (follow-up)

`ato ps` / `ato stop --all` do not track OCI sessions. Cleanup requires direct Podman commands.
Classification: **follow-up** (containers carry `io.ato.managed=true` label for future `ato stop` wiring).

### Scenario D — failure paths: complete ✅

| Fixture | Outcome |
|---------|---------|
| Bad image ref (nonexistent registry) | Typed E999 with "no such host" cause surfaced |
| Absolute bind mount `/etc/passwd` | Import rejected at parse time |
| `--privileged` flag | Import rejected at parse time |

---

## PR 11.7 — Minimal OCI session tracking: complete ✅

Implemented 2026-05-23, branch `feat/oci-provider-model`.

Fixes the Scenario C degradation from PR 11.6: `ato ps` / `ato stop --all` now track and stop OCI-managed sessions.

### Changes

| File | Change |
|------|--------|
| `adapters/runtime/oci_session_store.rs` (NEW) | `OciSessionRecord`, `OciSessionStore`, `stop_oci_session()`, `StopResult` |
| `adapters/runtime/mod.rs` | `pub(crate) mod oci_session_store;` |
| `executors/oci_multi_service.rs` | Write session record after containers start; delete after cleanup |
| `executors/install_sh_runner.rs` | `session_meta: Option<OciSessionMeta>` param; production call passes `docker-run-script` |
| `executors/oci_compose_runner.rs` | `session_meta: Option<OciSessionMeta>` param; production call passes `compose` |
| `cli/commands/close.rs` | `stop_all_oci_sessions()` wired into `--all` path |
| `cli/commands/ps.rs` | OCI sessions shown in text table and JSON output |

### Session record fields

Written to `${ATO_HOME}/oci-sessions/<session_id>.json` after all containers start
(`ATO_HOME` defaults to `~/.ato` when the env var is not set):

- `session_id`, `import_kind` (compose / docker-run-script), `provider` (podman)
- `source_path`, `source_hash`
- per-service: `container_name`, `image_ref`, `image_digest`, `platform`
- `network_name`, `endpoint` (main service URL), `created_at`
- `status`: running / stopped
- NOT written: secret values, generated secrets, raw DATABASE_URL with password

Deleted after `cleanup_services` completes (both success and failure paths).

### Lifecycle invariants

- `ato stop --all` reads running OCI sessions, stops containers via `podman stop/rm`, removes session network
- Persistent volumes: preserved (named volumes with no `/` in source)
- Ephemeral volumes: removed
- Idempotent: already-removed containers/networks return success
- Failed startup: no running session record persisted

### Test results

```
oci_session_store:  11/11 ✅  (includes 4 ATO_HOME isolation regression tests)
oci_multi_service:  18/18 ✅
install_sh:         11/11 ✅
oci_compose:        23/23 ✅
stop:               17/17 ✅
ps:                164/164 ✅
oci_provider:       20/20 ✅
docker_run_script:  19/19 ✅
oci_compose_lock:   18/18 ✅
```

### Remaining future work (not blocking PR)

- Full Desktop OCI session UX (Desktop shell `ato ps` / `ato stop` integration)
- Rich per-container status / logs from `ato ps`
- Podman machine-specific session cleanup on macOS shutdown
Receipt: `.tmp/aodd-receipts/oci-blinko-cleanup.yaml` updated to `result: complete`.

### Live Scenario C rerun (PR 11.7) ✅

Confirmed 2026-05-23 with real Podman 5.7.1 (macOS arm64, applehv machine running):

```
$ ato ps
Total: 1 capsule(s) (1 OCI)
  — ato-blin docker-run-script  🐳 running  oci/docker-run-script  http://127.0.0.1:33135/

$ ato stop --all
🐳 Stopping OCI session ato-blinko-aodd-aabcae6f (docker-run-script, 2 service(s))...
  ✅ Stopped container: ato-blinko-aodd-blinko-website-aabcae6f
  ✅ Stopped container: ato-blinko-aodd-blinko-postgres-aabcae6f
  🔗 Removed network: ato-blinko-aodd-aabcae6f

$ podman ps --filter label=io.ato.managed=true
(empty — 0 containers)

$ ato ps
No capsules found.
```

All lifecycle invariants confirmed live. No remaining Scenario C blockers.
