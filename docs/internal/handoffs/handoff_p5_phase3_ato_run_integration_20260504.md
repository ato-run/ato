---
title: "Handoff: Capsule Dependency Contracts P5 Phase 3 — `ato run` Integration"
status: handoff
date: "2026-05-04"
related:
  - "docs/rfcs/accepted/CAPSULE_DEPENDENCY_CONTRACTS.md"
  - "docs/plan_capsule_dependency_contracts_20260504.md"
---

# Handoff: P5 Phase 3 — `ato run` Integration

This document is the entry point for the next session implementing P5 phase 3 of the dependency-contract feature. Read it first.

## 1. Where we are

The accepted RFC `docs/rfcs/accepted/CAPSULE_DEPENDENCY_CONTRACTS.md` (v1.6) defines a uniform `capsule://` dependency grammar for the Ato runtime. Across the previous sessions we landed:

| Phase | Status | Commit | Module |
| --- | --- | --- | --- |
| P0 | ✅ closed | `97cfb973` | RFC v1.6 §4.4 / §5.3 normative; v0.3 normalizer migration |
| P1 | ✅ existing | `7f663453` | `[dependencies.*]` / `[contracts.*]` parser + AST |
| P1.5 | ✅ existing | `0a794d23` | manifest validation (target binding, credentials.default ban, etc.) |
| P2 | ✅ existing | `8bc51634` | `EnvOrigin` enum + `runtime_exports` excluded from `intrinsic_keys` |
| P3 verifier | ✅ | `5c9eb67d` | pure verifier in `capsule-core::dependency_contracts` (13 §9.1 rules) |
| P3 bridge | ✅ | `7509a0cf` | `verify_consumer_only` wired into lockfile generation |
| P4 | ✅ | `5433a740` | credential resolver + 3 channels + `RedactionRegistry` |
| P5 phase 1 | ✅ | `22ef0c68` | `endpoint` / `ready` / `orphan` / `teardown` building blocks |
| **P5 phase 2** | ✅ | `e4836ea5` | **orchestrator** with end-to-end mock-provider test |
| P6 | ✅ | `62d61a0c` + `f42cb948` | `dependency_derivation_hash` includes direct deps + `identity_exports` |

**Test status**: capsule-core 530/0、ato-cli 1208/0 — fully green across +64 new tests this session.

**v1 invariants enforced** (all 9 from RFC §3 end):

1. `runtime_exports` are NOT in v2 receipt `intrinsic_keys` (P2)
2. `credentials` are NOT in `instance_hash` or `dependency_derivation_hash` (P3 + P6)
3. credential values are NOT in lockfile (template form only) (P3)
4. `identity_exports` value strings reject `{{credentials.X}}` (P3 lock fail-closed)
5. credential literals in consumer manifest → lock fail (P3)
6. `{{env.X}}` in dep blocks must resolve from manifest top-level `required_env` (P3)
7. `{{credentials.X}}` is **never** literally substituted into argv/shell — Rule M1 channel only (P4 + orchestrator)
8. Reserved `unix_socket = "auto"` / `ready.type = http|unix_socket` → lock fail-closed (P3)
9. `credentials.<key>.default` → lock fail (P1.5 + P3)

## 2. What P5 phase 3 must do

The orchestrator from P5 phase 2 (`crates/ato-cli/src/application/dependency_runtime/orchestrator.rs::start_all`) is a complete, well-tested standalone module. Phase 3 wires it into the actual `ato run` command path so that when a user invokes `ato run` on a capsule with a `[dependencies.*]` block, the orchestrator launches the providers and injects their `runtime_exports` into the consumer's env.

### 2.1 Concrete tasks

1. **Identify the integration site in `ato run`**.
   - Entry: `crates/ato-cli/src/cli/dispatch/run.rs::execute_run_like_command` (~682 LOC)
   - The function loads the consumer manifest and lockfile, prepares the runtime, and launches the consumer target. Phase 3 hooks between lockfile load and consumer launch.

2. **Read the lockfile** (already happens) and **detect dependency-contract entries**.
   - Existing reader: `manifest_external_capsule_dependencies` in `capsule-core::lockfile`
   - The lockfile field is `capsule_dependencies: Vec<LockedCapsuleDependency>`. Entries with `contract: Some(...)` and the new `identity_exports` field populated are dep-contract entries; entries without `contract` are the old `external_capsule` adapter's responsibility (kept untouched).
   - Build a `capsule_core::dependency_contracts::DependencyLock` from these entries (or, alternatively, expose a conversion helper in capsule-core).

3. **Materialize provider capsules** so we can hand each one's parsed manifest + a local filesystem root to the orchestrator.
   - The existing path that already does this for the old adapter: `crates/ato-cli/src/adapters/runtime/external_capsule/cache.rs` extracts artifacts to a cache dir.
   - Reuse that machinery (or call into it) to produce, for each dep-contract entry: `(parsed_manifest: CapsuleManifest, provider_root: PathBuf, resolved: String)`.
   - This is the moment when the **full** verifier (`verify_and_lock`, not the consumer-only bridge) should run, since we now have provider manifests in hand. Failing here is fail-closed and aborts run.

4. **Construct `OrchestratorInput`** and call `start_all`.
   - `consumer`: parsed `CapsuleManifest`
   - `lock`: the `DependencyLock` from step 2/3
   - `providers`: `BTreeMap<String, OrchestratorProvider>` from step 3
   - `ato_home`: resolve from existing Ato config (search `ato_home_dir()` or similar in the codebase)
   - `parent_package_id`: consumer's package id (typically `<name>@<version>`)
   - `host_env`: `&ProcessHostEnv`
   - `redaction`: a process-local `Arc<RedactionRegistry>` (or use `dependency_credentials::global_redaction()`)
   - `session_pid`: `std::process::id() as i32`
   - `default_ready_timeout`: e.g. `Duration::from_secs(30)`
   - `ready_probe_interval`: `Duration::from_millis(200)`

5. **Inject `runtime_exports` into the consumer's env** before launching the consumer target.
   - `RunningGraph::runtime_exports(alias)` returns `Option<&BTreeMap<String, String>>`
   - For each consumer target's env, expand `{{deps.<alias>.runtime_exports.<key>}}` using these resolved values
   - The env entries injected must carry origin = `EnvOrigin::DepRuntimeExport(alias)` so the v2 receipt `intrinsic_keys` excludes them (RFC §7.4.1, P2 invariant)
   - Same for `{{deps.<alias>.identity_exports.<key>}}` — origin = `DepIdentityExport(alias)`

6. **Lifecycle integration**:
   - Hold the `RunningGraph` for the lifetime of the consumer process
   - On consumer normal exit / Ctrl-C / crash: call `RunningGraph::teardown(grace)` (e.g. 10s grace)
   - `Drop` on `RunningGraph` provides best-effort cleanup; explicit `teardown` is preferred so errors propagate

7. **Provider stdout/stderr → log writer with redaction**.
   - The orchestrator currently sets `Stdio::piped()` on the provider's stdout/stderr but does not yet wire a reader thread.
   - Phase 3 task: spawn a reader thread per dep that reads provider output line-by-line and writes through `RedactionRegistry::redact` to a log file or stderr.
   - Log path: `<ato_home>/logs/<parent_pkg_id>/<dep_alias>/<timestamp>.log` (or follow whatever convention the existing `app_control` log paths use).

### 2.2 Out of scope for phase 3

These remain deferred regardless of phase 3:
- `[provision]` block execution (provider self-bootstraps; or pre-existing state)
- Stdin / EnvVar materialization channels (TempFile is the v1 default)
- HTTP / unix_socket ready probes (lock-fail-closed in capsule-core)
- Real Postgres E2E with WasedaP2P — that's phase 7

## 3. Key code references

### 3.1 The orchestrator (the thing you're integrating)

`crates/ato-cli/src/application/dependency_runtime/orchestrator.rs`

```rust
pub struct OrchestratorInput<'a> {
    pub lock: &'a DependencyLock,
    pub providers: BTreeMap<String, OrchestratorProvider>,
    pub consumer: &'a CapsuleManifest,
    pub ato_home: PathBuf,
    pub parent_package_id: String,
    pub host_env: &'a dyn HostEnv,
    pub redaction: Arc<RedactionRegistry>,
    pub session_pid: i32,
    pub default_ready_timeout: Duration,
    pub ready_probe_interval: Duration,
}

pub struct OrchestratorProvider {
    pub manifest: CapsuleManifest,
    pub provider_root: PathBuf,
    pub resolved: String,
}

pub fn start_all(input: OrchestratorInput<'_>) -> Result<RunningGraph, OrchestratorError>;

impl RunningGraph {
    pub fn deps(&self) -> &[RunningDep];
    pub fn runtime_exports(&self, alias: &str) -> Option<&BTreeMap<String, String>>;
    pub fn teardown(self, grace: Duration) -> Result<(), TeardownError>;
}
```

The end-to-end test in the same file (`start_all_spawns_provider_runs_ready_probe_and_tears_down`) shows exactly how to construct the input and consume the output. **Read that test first** when starting phase 3 — it's the canonical example.

### 3.2 The verifier

`crates/capsule-core/src/foundation/dependency_contracts/lock.rs`

```rust
pub fn verify_and_lock(input: DependencyLockInput<'_>) -> Result<DependencyLock, LockError>;
pub fn verify_consumer_only(consumer: &CapsuleManifest) -> Result<(), LockError>;
```

Phase 3 should call `verify_and_lock` (the full version) at run time once provider manifests are materialized. The consumer-only bridge already runs at lockfile generation time via `97cfb973` / `7509a0cf`.

### 3.3 The credential pipeline

`crates/ato-cli/src/application/dependency_credentials.rs`

```rust
pub trait HostEnv { fn get(&self, key: &str) -> Option<String>; }
pub struct ProcessHostEnv;  // production
pub struct ResolvedSecret { /* zeroize on drop */ }
pub fn resolve_credential_template(...) -> Result<ResolvedSecret, CredentialError>;
pub enum MaterializationChannel { TempFile { state_dir, cred_key }, EnvVar { ... }, Stdin }
pub fn materialize_credential(...) -> Result<MaterializedRef, CredentialError>;
pub struct RedactionRegistry { register, redact }
pub fn global_redaction() -> &'static RedactionRegistry;
```

The orchestrator already uses these internally; phase 3 only needs to instantiate `RedactionRegistry` (or use the global) and pass it through.

### 3.4 The integration point (where to hook in)

`crates/ato-cli/src/cli/dispatch/run.rs::execute_run_like_command` — the existing `ato run` entry point, ~682 LOC. Phase 3 should add a code path between lockfile load and consumer launch that builds `OrchestratorInput` and threads `RunningGraph` through to the consumer's launch.

The **existing** external-capsule adapter is at:
- `crates/ato-cli/src/adapters/runtime/external_capsule/mod.rs` — entry point
- `crates/ato-cli/src/adapters/runtime/external_capsule/cache.rs` — artifact extraction
- `crates/ato-cli/src/adapters/runtime/external_capsule/bindings.rs` — env merging

Phase 3 does **not** delete this adapter — it handles the legacy `[external_dependencies]` shape and `injection_bindings`. The new orchestrator runs **alongside** it for entries where `LockedCapsuleDependency.contract` is `Some(...)`.

### 3.5 Existing `app_control` desktop bootstrap

`crates/ato-cli/src/app_control.rs` — `materialize_managed_services` / `orchestrate_managed_services` / `stop_managed_services` are the **ato-desktop bootstrap** subsystem (Postgres, ai-gateway, etc. for desktop bundle). They predate the RFC and are explicitly out of scope for the dep-contract migration. Don't refactor them. The orchestrator coexists.

## 4. Open design questions for phase 3

Decide before coding (or document the choice in the PR):

1. **Lockfile → DependencyLock conversion.**
   - Option A: add `pub fn dependency_lock_from_capsule_lock(lock: &CapsuleLock, providers: &...) -> DependencyLock` to `capsule-core`
   - Option B: have ato-cli construct it manually (it already has both `LockedCapsuleDependency` and `LockedDependencyEntry` shapes)
   - Recommend Option A — keeps capsule-core as the source of truth on the schema, ato-cli stays a thin caller.

2. **Provider manifest fetch strategy.**
   - Option A: fully artifact-based — pull the `.capsule` artifact, extract `capsule.toml`, parse. The existing `external_capsule::cache` does this. Reuse.
   - Option B: registry-side `/manifest` endpoint that returns just the TOML. Out of scope for phase 3 (registry change).
   - Use Option A for phase 3.

3. **Where does `parent_package_id` come from?**
   - The consumer manifest has `name` and `version`. Compose as `<name>@<version>` for now.
   - Alternative: if Ato has a canonical package id concept (e.g. content-addressed), use that.
   - Document the choice in the orchestrator wiring layer; this is consumer-side and can be revisited without breaking lock identity.

4. **Stdio reader thread lifecycle.**
   - Each provider gets a thread that reads stdout/stderr line-by-line through `RedactionRegistry::redact`.
   - Threads should detach (background) and self-terminate when the read returns 0 bytes (process exited).
   - Log destination: TBD — match existing `app_control` log conventions or create a new `<ato_home>/logs/deps/<alias>/` path.

5. **Teardown trigger.**
   - On consumer normal exit: call `teardown` after consumer wait.
   - On Ctrl-C: install a signal handler that triggers teardown. The existing `dispatch/run.rs` likely already does this for the consumer; piggyback on it.
   - On panic: `Drop` on `RunningGraph` is the safety net.

## 5. Test strategy for phase 3

A: **Unit tests** at the wiring layer:
- Conversion lockfile → DependencyLock with synthetic entries
- `parent_package_id` derivation
- Provider manifest extraction from a fake artifact (using existing cache paths)

B: **Integration test** end-to-end with a mock provider:
- Reuse the mock fixture from `orchestrator.rs::tests::start_all_spawns_provider_runs_ready_probe_and_tears_down`
- Wrap it with the actual `execute_run_like_command` path
- Assert: consumer launches with `runtime_exports.MODE = "test"` in its env
- Assert: on consumer exit, `teardown` runs and sentinel is swept

C: **Real Postgres E2E** (= phase 7, separate session): WasedaP2P + `ato/postgres` provider capsule. Don't include in phase 3 PR.

## 6. Recommended PR structure

PR 8: `feat(ato-cli): integrate dep-contract orchestrator into ato run`

- Add `dependency_lock_from_capsule_lock` helper to capsule-core (if Option A in §4.1)
- New `dispatch::run::launch_dep_contracts` (or similar) that:
  - reads lockfile
  - filters dep-contract entries
  - calls `external_capsule::cache::ensure_artifact` per dep to get provider artifact
  - parses provider `capsule.toml` from artifact root
  - builds `OrchestratorInput` and calls `start_all`
- Inject `RunningGraph::runtime_exports` into consumer env
- Wire teardown into existing consumer exit path
- Stdio reader threads with redaction filter
- Unit + integration tests

Estimated scope: 800–1200 LOC + 6–10 tests. Should fit in one PR.

## 7. Useful commands

```bash
# Full test suite
cargo test -p capsule-core --lib
cargo test -p ato-cli --lib

# Just the dep-contract subsystem
cargo test -p capsule-core --lib foundation::dependency_contracts
cargo test -p ato-cli --lib dependency_contracts dependency_credentials dependency_runtime

# Format (memory: only Koh0920 author, no Co-Authored-By)
cargo fmt -p capsule-core -p ato-cli

# Recent commits in this feature line
git log --oneline aa55a5a0..HEAD -- crates/capsule-core crates/ato-cli docs/rfcs/accepted/CAPSULE_DEPENDENCY_CONTRACTS.md
```

## 8. Reading order for the next agent

1. **This file** (`docs/handoff_p5_phase3_ato_run_integration_20260504.md`)
2. RFC §3 (the v1 scope and 9 invariants)
3. RFC §10.2 (startup sequence) and §10.4 (teardown / orphan)
4. The orchestrator end-to-end test in `orchestrator.rs::tests` (the canonical usage)
5. The current `dispatch/run.rs::execute_run_like_command` (find the integration site)

After that you should have everything needed to design and implement phase 3 in a single focused PR.

## 9. Things to remember

- **No `Co-Authored-By` trailers** in commits (memory: this repo lists only Koh0920).
- Keep the brick boundary: capsule-core stays pure (no network/fs), ato-cli does the I/O.
- The orchestrator is **already pure** with respect to fetching — phase 3's job is the I/O wrapper around it.
- v1 backward compatibility is **not required** (per memory + repo policy).
- All 9 v1 invariants are now machine-enforced; phase 3 must not regress any of them.
