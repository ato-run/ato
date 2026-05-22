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
| `capsule-core` | `docker_run_script` | 18 | Pure importer unit tests |
| `capsule-core` | `oci_compose_lock` | 18+ | Lock persistence tests |
| `capsule-core` | `compose_import` | 20+ | Compose importer tests |
| `capsule-core` | `derive` | 32 | ExecutionPlan identity tests |
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
   `-p capsule-core` with module filters.

2. `install_aliases_in_same_scope_are_rejected` test in
   `cli::commands::run::preflight::tests` fails pre-existing (unrelated to OCI).

---

## Running the validation suite

```sh
# Format
cargo fmt --all

# Type-check
cargo check -p capsule-core -p ato-cli

# Unit tests
cargo test -p capsule-core docker_run_script --lib
cargo test -p capsule-core compose_import --lib
cargo test -p capsule-core oci_compose_lock --lib
cargo test -p capsule-core derive --lib
cargo test -p ato-cli install_sh --lib
cargo test -p ato-cli oci_compose --lib
cargo test -p ato-cli oci_multi_service --lib
cargo test -p ato-cli oci_single_target --lib
cargo test -p ato-cli oci_provider --lib
cargo test -p ato-cli podman_probe --lib

# Optional: real Podman smoke (requires Podman + machine on macOS)
ATO_TEST_REAL_PODMAN=1 cargo test -p ato-cli real_podman -- --ignored --nocapture
```
