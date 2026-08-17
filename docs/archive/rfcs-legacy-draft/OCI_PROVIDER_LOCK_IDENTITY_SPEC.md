---
title: "OCI Provider, Lock, and Identity Spec"
status: draft
date: 2026-05-22
author: "@Koh0920"
ssot:
  - "crates/capsule/src/foundation/types/oci.rs"
  - "crates/capsule/src/contract/lock_runtime.rs"
  - "crates/capsule/src/engine/execution_identity/mod.rs"
  - "crates/ato-cli/src/adapters/runtime/oci_provider.rs"
related:
  - "accepted/CAPSULE_SPEC.md"
  - "accepted/ORCHESTRATION_AND_SERVICES.md"
  - "accepted/EXECUTIONPLAN_ISOLATION_MODEL.md"
  - "draft/ENGINE_AND_WORKLOAD_MODEL.md"
---

# OCI Provider, Lock, and Identity Spec

## 1. Overview

OCI is not a separate execution world in Ato. A capsule declares portable OCI
intent, the lock pins image resolution, execution identity hashes the launch
envelope, a provider materializes that envelope on the host, the session records
live runtime ids, and the receipt audits what was resolved and enforced.

```text
Capsule        image refs, service graph, state, policy intent
Lock           image digest, platform, importer input hash
Identity       canonical OCI launch envelope
Provider       materialize/start/stop/logs/inspect through Podman or compatible APIs
Session        container id, network id, host port, volume id, health
Receipt        resolved facts, policy enforcement result, redacted diagnostics
```

## 2. Scope

### In scope

- Keep the public capsule contract as `runtime = "oci"`.
- Define the lock shape for resolved OCI images.
- Define the OCI launch envelope fields that participate in execution identity.
- Define provider terminology and the first adapter boundary.
- Define policy enforcement modes and fail behavior.
- Define state binding identity boundaries for OCI services.

### Out of scope

- Direct Podman execution.
- Docker Compose or install script importers.
- Desktop-specific provider controls.
- Native image store, native networking, or Youki integration.

## 3. Design

### 3.1 Public contract

Manifests declare OCI portability, not a concrete host provider:

```toml
[targets.app]
runtime = "oci"
image = "ghcr.io/acme/app:latest"
port = 3000

[services.main]
target = "app"
```

Host policy and config choose the provider. v1 official provider semantics are
Podman-first, but the manifest must not say `runtime = "podman"`.

### 3.2 Provider terminology

Use `OciProvider` for host materialization. Reserve `Engine` for the broader
external engine/workload contract described by `ENGINE_AND_WORKLOAD_MODEL.md`.

Provider semantics are coarse identity inputs:

```json
{
  "kind": "podman",
  "mode": "rootless",
  "substrate": "podman-machine",
  "policy_profile": "oci-podman-v1"
}
```

The following are diagnostic only and must not enter execution identity:

- exact provider version
- machine id or VM name
- container id
- network id
- generated volume id
- allocated host port

### 3.3 Provider readiness

Provider readiness is a host placement check. It answers whether the selected
host provider can materialize an OCI launch envelope; it does not resolve image
digests, pull images, create containers, start services, or allocate live
session resources.

The first selector defaults to the official v1 provider, Podman. Host config,
environment, and policy-based selection are future work; Docker-compatible
providers may remain behind the adapter boundary for migration and tests.

Readiness has two call-site modes:

| Mode | Use | Missing or unready provider |
| --- | --- | --- |
| `Required` | Before OCI materialization or any operation that depends on a ready host provider. | Return the typed OCI provider error and fail the operation. |
| `BestEffort` | Diagnostics, import, discovery, or UI inventory where probing should not block the parent operation. | Return the typed diagnostic outcome without failing the parent operation. |

Typed readiness failures include:

- `oci_provider_missing` for a missing Podman binary.
- `oci_provider_not_ready` for an installed provider that cannot currently
  materialize, such as a required Podman machine that is not running.
- `oci_provider_probe_failed` for command execution or parsing failures that
  prevent a trustworthy inventory.
- `oci_provider_capability_unsupported` when a required capability such as
  rootless operation is unsupported or ambiguous.

Readiness inventory may record Podman version and machine status for diagnostics,
but those facts remain outside execution identity. Image digest resolution
remains a later phase after the provider boundary is in place.

### 3.4 Lock resolution

The canonical lock location for OCI image resolution is
`resolution.oci_images.<target-label>`:

```json
{
  "resolution": {
    "oci_images": {
      "app": {
        "declared_ref": "ghcr.io/acme/app:latest",
        "resolved_digest": "sha256:...",
        "platform": {
          "os": "linux",
          "architecture": "arm64"
        },
        "importer_input_hash": "blake3:..."
      }
    }
  }
}
```

The old `resolution.resolved_targets[].image` string remains the declared image
ref for compatibility. It is not sufficient for reproducible OCI launch because
mutable tags and selected platform are not pinned there.

### 3.5 Execution identity

OCI identity is the launch envelope, not only the image digest. It includes:

- resolved image digest set
- selected platform
- service graph shape
- command and entrypoint overrides
- environment key closure and secret reference shape
- state binding and mount shape
- container port exposure policy
- network aliases and policy hashes
- readiness probe shape
- coarse provider semantics label

It excludes:

- secret values
- allocated host port
- container id
- network id
- generated volume id
- host absolute state path

For non-OCI launches, the optional OCI envelope is omitted from the v2 JCS
projection so existing non-OCI identity bytes remain compatible. If a future
change cannot preserve this omission rule, it must bump the identity schema.

### 3.6 State boundary

OCI persistent state uses the existing `[state]` and
`services.*.state_bindings` model. Execution identity records only logical mount
shape:

```json
{
  "state": "postgres_data",
  "target": "/var/lib/postgresql/data",
  "readonly": false,
  "durability": "persistent"
}
```

State identity does not include volume contents, generated volume ids, host
absolute paths, uid/gid mappings, or provider-specific volume names. If Ato
launches from a state snapshot, the snapshot hash/ref is included explicitly.

### 3.7 Policy behavior

Provider policy behavior is fail-closed unless the capsule or operator chooses a
weaker mode:

```text
strict  provider cannot enforce a requested policy -> launch fails
loose   provider records a warning and requires consent
off     provider records best-effort diagnostics only
```

Providers must not claim enforcement they cannot provide. For example, if a
Podman v1 provider cannot enforce a domain egress allowlist, the receipt records
that gap and strict mode fails before launch.

### 3.8 Image resolution lifecycle

Image resolution converts a declared image ref (which may include a mutable tag)
into a content-addressed, platform-specific identity. It is a distinct step from
image pull and must complete before execution identity is computed.

**Lifecycle:**

```text
1. Manifest inspect    podman manifest inspect <declared_ref>
2. Platform selection  pick child entry matching requested_platform
3. Digest extraction   record child manifest digest (platform-specific, not index digest)
4. Lock write          write declared_ref + resolved_digest + platform to resolution.oci_images
5. Execution identity  lock digest + provider semantics label included in OCI envelope
```

**Required vs BestEffort:**

| Mode | Unresolved image | Malformed ref | Unsupported platform |
|------|-----------------|---------------|---------------------|
| `Required` | Typed `ImageResolveFailed` error — operation fails | Typed `ImageRefMalformed` error | Typed `ImagePlatformUnsupported` error |
| `BestEffort` | Diagnostic result without failing parent operation | Same, reported in failures collection | Same |

**Mutable tags:**

A mutable tag (e.g. `latest`, `main`, a semver without an explicit digest) may
be accepted only after being resolved to a digest in the lock. If a mutable-tag
image cannot be resolved — for example because the provider is offline — the
operation fails in `Required` mode. In `BestEffort` mode the unresolved ref is
recorded in the failure collection; it must not be written to the lock.

A digest ref (e.g. `ghcr.io/acme/app@sha256:...`) round-trips without forced
mutation and suppresses the mutable-tag warning.

**Multi-platform manifests:**

If the manifest is a multi-platform index and no `requested_platform` is given,
resolution fails if more than one candidate entry is present. Auto-picking an
arbitrary platform is forbidden because the choice must be deterministic and
auditable.

If the manifest is a single-arch image (no `manifests` array in inspect output)
and the ref already carries an embedded digest, the ref is usable provided a
`requested_platform` is explicitly supplied. Without an explicit platform there
is no reliable way to record the platform; resolution fails.

**What resolution does not do:**

Image resolution explicitly does not:
- pull the image layers to the local store
- create containers
- start services
- allocate host ports or volumes

Pull and materialization are deferred to a later slice (PR 7 in the rollout
above) after the provider boundary and lock update contract are solid.

**Why resolved digest is necessary but not sufficient:**

A resolved image digest pins the exact image content. It is a required input to
execution identity. However, the digest alone does not capture:

- selected platform (arm64 and amd64 variants of the same image have the same
  index digest but different child digests)
- environment variable closure and secret ref shape
- state binding and mount shape
- network policy and port exposure intent
- provider semantics

Execution identity therefore includes the full OCI launch envelope, of which the
resolved digest is one field.

### 3.9 ExecutionPlan policy envelope

When `compile_execution_plan` processes a manifest target with `runtime = "oci"`, it
attaches an `OciPolicyEnvelope` to the `ExecutionPlan`. This envelope records the
plan-time-known OCI policy facts required for consent hashing and provider readiness
checks.

**Shape:**

```rust
pub struct OciPolicyEnvelope {
    pub declared_image_ref: String,
    pub resolved_image: Option<OciImageResolution>, // from lock if resolved
    pub port_exposure: Option<u16>,
    pub egress_allow: Vec<String>,
    pub policy_mode: OciPolicyMode,           // defaults to Strict
}

pub enum OciPolicyMode { Strict, Loose, Off }
```

**Population rules:**

| Field | Source |
|-------|--------|
| `declared_image_ref` | `targets.<label>.image` in manifest |
| `resolved_image` | `resolution.oci_images.<label>` in lock (absent if no lock) |
| `port_exposure` | `targets.<label>.port` in manifest |
| `egress_allow` | `[network].egress_allow` in manifest |
| `policy_mode` | Always `Strict` at compile time (configurable in a later slice) |

**What the envelope does not store:**

- Container id, network id, volume id — live runtime state only
- Host-allocated port — chosen at materialization time
- Secret values — never in plan

**Tier:**

OCI targets compile to `ExecutionTier::Tier3` (containerized OCI). As a result,
`lock_required = false` and `integrity_required = false` for this tier. These will
be tightened in the image pull / materialization slice when lock presence becomes
a pre-execution gate.

**Consent hash inclusion:**

The `OciPolicyEnvelope` is part of the `ExecutionPlan` serialization. It is
included in the policy segment hash computed by `compute_policy_segment_hash`.
Changes to `declared_image_ref`, `egress_allow`, `port_exposure`, or `policy_mode`
all change the consent key. The `resolved_image` field also changes the consent key
when a digest is resolved, ensuring that a tag-to-digest resolution event requires
fresh consent.

## 4. Interfaces

The first code surface is:

- `OciImageResolution` for lock-pinned image facts.
- `OciLaunchEnvelope` for identity input.
- `OciProviderSemantics` for coarse materialization semantics.
- CLI-side `OciProvider` trait for probe, image resolution/pull, network,
  container, logs, stop, remove, and inspect.

Actual Podman command execution is a later implementation phase. The existing
Bollard path remains a compatibility adapter behind the new boundary until it
can be migrated or removed.

## 5. Security

- Secret values never enter lock, identity, session, or receipt.
- Strict policy mode fails closed if the provider cannot enforce a requested
  boundary.
- Provider version and machine identifiers are receipt/session diagnostics, not
  cache keys.
- Host-local paths and generated provider ids are excluded from the portable
  identity envelope.

## 6. Rollout

```text
PR 0: RFC and model agreement
PR 1: OCI lock schema and launch envelope schema
PR 2: OciProvider boundary and fake provider tests
PR 3: PodmanProvider probe and typed errors
PR 4: provider selector and readiness mode wiring
PR 5: image resolve/pull and lock update
PR 6: ExecutionPlan, ExecutionGraph, Identity connection
PR 7: single-target Podman execution
PR 8: multi-service Blinko
PR 9: Docker Compose subset importer (pure)
PR 10: CLI wiring — --oci-compose flag, image resolution, lock/plan/run
PR 10.5: hardening — failure-path tests, diagnostics, opt-in real Podman smoke
PR 11: install.sh / docker-run intent extractor (future)
```

## 7. References

- `docs/rfcs/accepted/CAPSULE_SPEC.md` - existing `runtime = "oci"` surface.
- `crates/capsule/src/routing/router/services.rs` - service graph already
  supports OCI service constraints.
- `crates/capsule/src/engine/runtime/oci.rs` - current Docker-compatible
  implementation, to be isolated behind provider boundaries.
- `crates/ato-session-core/src/record.rs` - session records already store OCI
  container ids and host port mappings.

## 8. PR 7 — Single-target Podman OCI execution

**Scope**: `oci_single_target.rs` executor, PodmanProvider lifecycle methods,
FakeOciProvider, helper utilities.

### 8.1 Execution gate invariant

PR 6 moved the safety invariant from "ExecutionPlan rejects OCI" to "execution
path cannot proceed without passing the explicit gate". The gate requires:

1. `OciPolicyEnvelope` present in the compiled `ExecutionPlan`.
2. `resolved_image.digest` present (lock must have been run before `ato run`).
3. `PodmanProvider` readiness in `Required` mode.
4. Policy mode acceptable: `Strict` fails if unenforced policies are declared.

If any condition fails, a typed `OciProviderError` is returned. No fallback to
Bollard/Docker-compatible execution occurs.

### 8.2 Official vs legacy execution path

| Path | Module | Status |
|------|--------|--------|
| `PodmanProvider` + `oci_single_target` | `executors/oci_single_target.rs` | Official |
| Bollard/Docker-compatible | `executors/oci.rs` | Legacy — do not route new OCI execution here |

The legacy path is retained for backward compatibility only. It has a `//! LEGACY:`
doc comment at the top of the file. New OCI capsule execution must not route
through it.

### 8.3 Why resolved image digest is required before execution

Resolved digest at the lock layer ensures:
- **Reproducibility**: the same lock always pulls the same image bytes.
- **Consent identity**: the digest is part of the consent hash, so UI consent
  is anchored to a specific image, not a mutable tag.
- **Receipt auditability**: the receipt can record exactly what was run.

Without a resolved digest, `ato run` for an OCI target returns a typed error
pointing the user to `ato lock`.

### 8.4 Execution identity exclusions (PR 7)

Live runtime state generated during PR 7 execution is recorded only in
Session/Receipt, not in Execution Identity:

- `container_id` — session record only
- `allocated_host_port` — session record / URL display only
- `network_id` — future multi-service slice
- `volume_id` — future state-binding slice
- `Podman machine id` — provider diagnostics / receipt only

### 8.5 Policy behavior (PR 7)

| `policy_mode` | `egress_allow` | Outcome |
|---------------|----------------|---------|
| `Strict` | non-empty | `OciExecutionGateFailed` — execution blocked |
| `Strict` | empty | Gate passes |
| `Loose` | non-empty | Warning to stderr; execution proceeds with gap |
| `Loose` | empty | Gate passes |
| `Off` | any | Gate always passes |

`Strict` is the default for new manifests. The semantics of `Strict` in PR 7
are: _provider must be able to enforce all declared policies_. Podman rootless
cannot enforce network egress allowlists, so `Strict` + non-empty `egress_allow`
always fails.

### 8.6 Multi-service / Blinko

Multi-service OCI (app + postgres, internal networks, named volumes, state
bindings) is deferred to PR 8. PR 7 supports exactly one OCI target per
invocation.

### 8.7 Compose / install.sh importer

Docker Compose subset importer and `install.sh` intent extractor are deferred
to PR 9. The execution model (digest lock, provider gate, identity boundary,
receipt) must be solid before adding importer-generated capsule manifests.

---

## 9. PR 8 — Multi-Service Podman OCI Execution

### 9.1 Scope

PR 8 supports an explicit OCI service graph declared through the existing
`[services]` model. The motivating example is Blinko: an app container
(`blinkospace/blinko:latest`) and a database container (`postgres:14`) where
the app must reach the database by an internal alias before it starts.

Out of scope for PR 8:
- Docker Compose or `install.sh` importers (PR 9)
- Privileged containers, host network, device mounts
- Restart-always policy
- Swarm/deploy semantics

### 9.2 Service graph execution

Services are started in topological order derived from `depends_on`.

```
1. Resolve all OCI images → require digest for every service (Required mode)
2. Apply aggregate policy gate (Strict/Loose/Off)
3. Create session-scoped Podman network
4. For each service (in dependency order):
   a. Pull resolved image
   b. Create container with Ato-owned labels and network alias
   c. Start container
   d. Wait readiness (TCP or HTTP probe)
5. Record session state and return receipt
```

Failure at any step triggers cleanup of all resources created so far.

### 9.3 Podman network and naming

One Podman network per Ato session, named `ato_<session_id_prefix>`.

Container names are session-scoped: `ato_<session_prefix>_<service_label>`.

Network aliases are the logical service labels (e.g., `db`, `app`). Internal
traffic uses these aliases; the alias is never exposed externally.

Required Ato-owned labels on all resources:

```
io.ato.managed = true
io.ato.session_id = <id>
io.ato.execution_id = <id>
io.ato.provider = podman
io.ato.target = <service_label>
```

### 9.4 State bindings → Podman bind mounts

Existing `[state.*]` entries with `state_bindings` are mapped to bind mounts
on the container at execution time.

- `durability = "persistent"` → volume path is preserved across sessions; not
  deleted on failure.
- `durability = "ephemeral"` → temp directory path is deleted on session stop
  or failure.

Volume path is recorded in the session record keyed by the state name.

Execution identity includes the state binding *shape* (state key, target path,
durability), not the generated volume path or contents.

### 9.5 Readiness probes

Container exit-wait is NOT used as a readiness signal for long-running
services. Two probe types are supported:

| Probe type | Use case |
|------------|----------|
| TCP | database port liveness (`postgres:5432`) |
| HTTP | app endpoint liveness (`blinko:1111/health`) |

Probe parameters are resolved from the service `port` declaration and optional
capsule manifest hints. If no explicit probe is configured, TCP probe on the
declared port is attempted.

If readiness times out, `OciProviderError::HealthcheckTimeout` is returned,
logs for the failed service are collected, and all resources are cleaned up.

### 9.6 Ports

Only the user-facing service (the one with the declared manifest `port`) gets
a host port. Database ports are internal only.

| Field | Location |
|-------|----------|
| Container port (stable) | Execution identity |
| Auto-allocated host port | Session record and receipt URL only |

Host port must not appear in execution identity or receipt identity hash.

### 9.7 Generated secret wiring

For Blinko-style `app + postgres`, connection secrets (`POSTGRES_PASSWORD`,
`NEXTAUTH_SECRET`, `DATABASE_URL`) are generated per-session.

Rules:
- Secret values never enter execution identity.
- Secret values never enter receipt (only redacted key names and derivation
  shape are recorded).
- `ATO_SERVICE_<LABEL>_HOST` is the network alias (stable), not `127.0.0.1`.
- `ATO_SERVICE_<LABEL>_PORT` is the container port, not the host port.
- `DATABASE_URL` template shape is part of identity; the embedded password
  value is not.

### 9.8 Policy behavior at graph level

Policy mode is aggregated across the service graph.

| `policy_mode` | `egress_allow` | Outcome |
|---------------|----------------|---------|
| `Strict` | non-empty | Execution blocked |
| `Strict` | empty | Gate passes |
| `Loose` | non-empty | Diagnostic warning; execution proceeds |
| `Loose` | empty | Gate passes |
| `Off` | any | Always passes |

`Strict` is the default for new manifests.

### 9.9 Session and receipt

Session records:
- provider kind
- network name / id
- per-service: container id, container name, image digest, platform, readiness status
- main service host port
- volumes keyed by state name

Receipt records:
- declared image refs
- resolved digests
- provider semantics label
- policy enforcement result
- redacted env key names and derivation shape

Receipt never contains: secret values, generated passwords, allocated host
port in identity, Podman machine id in identity.

### 9.10 Compose / install.sh importer

Docker Compose subset importer and `install.sh` intent extractor remain
deferred to PR 9.

---

## §10 PR 9 — Docker Compose Subset Importer

### 10.1 Overview

PR 9 adds a **pure Docker Compose subset importer** in
`capsule::routing::importer::compose`. The importer converts a
`docker-compose.yml` / `compose.yml` into an Ato OCI service graph projection
without executing Docker Compose, shelling out, or performing any host I/O
beyond reading the file text supplied through `ComposeImportInput`.

### 10.2 Entry points

| Function | Description |
|---|---|
| `detect_compose_candidate(dir)` | Returns the first candidate file found in priority order |
| `import_compose(input)` | Pure converter; returns `ComposeImportOutput` or `ComposeImportError` |
| `ComposeImportOutput::to_orchestration_plan()` | Converts output to `OrchestrationPlan` for use with the PR 8 executor |

### 10.3 File discovery priority

1. `docker-compose.yml`
2. `docker-compose.yaml`
3. `compose.yml`
4. `compose.yaml`

### 10.4 Supported Compose subset

| Field | Support |
|---|---|
| `services.<n>.image` | ✅ Required |
| `services.<n>.command` | ✅ |
| `services.<n>.entrypoint` | ✅ |
| `services.<n>.environment` (map + list) | ✅ |
| `services.<n>.ports` | ✅ — container port only; host port discarded |
| `services.<n>.volumes` (named) | ✅ → Ato state binding |
| `services.<n>.volumes` (relative bind `./`) | ⚠️ Allowed with warning |
| `services.<n>.volumes` (absolute bind `/`) | ❌ Hard rejected |
| `services.<n>.depends_on` (list + map) | ✅ |
| `services.<n>.healthcheck` | ✅ Conservative |
| `services.<n>.container_name` | ⚠️ Source metadata only |
| `services.<n>.build` (no image) | ❌ Rejected |
| `services.<n>.privileged: true` | ❌ Rejected |
| `services.<n>.network_mode: host` | ❌ Rejected |
| All other keys | ⚠️ Reported in `unsupported_features` |

### 10.5 Mapping rules

- **Service name → logical alias**: the Compose service key becomes the Ato
  network alias and logical service label. `container_name` is stored in
  `source_container_name` as metadata only and is never used as the runtime
  container name.
- **Ports**: only the container port is preserved. Host port is discarded
  because Ato auto-allocates host ports and records them in Session/Receipt
  only.
- **Named volumes**: mapped to Ato state bindings with `StateBindingKind::Named`.
- **Absolute bind mounts**: hard rejected with
  `ComposeImportError::AbsoluteBindMountRejected`.
- **Relative bind mounts**: allowed with warning; recorded as
  `StateBindingKind::ProjectRootBind`.
- **`depends_on` list**: `DependencyCondition::ServiceStarted`.
- **`depends_on` map + `condition: service_healthy`**: `DependencyCondition::ServiceHealthy`.
- **Unknown `depends_on` target**: `ComposeImportError::UnknownDependency`.
- **Dependency cycles**: detected via `startup_order_from_dependencies`;
  `ComposeImportError::DependencyCycle`.

### 10.6 Env and secret handling

Keys containing `PASSWORD`, `SECRET`, `TOKEN`, `PASSWD`, `CREDENTIAL`,
`AUTH`, `CERT`, or `_KEY` (case-insensitive) are classified as secret-like
(`is_secret_like: true`) and a warning is emitted. Callers must use this flag
to redact values from Receipt output. The literal value is preserved in the
import projection to allow the caller to substitute a generated secret.

`RequiredExternal` env entries (key without value, or `KEY` in list form) are
passed through to `ResolvedTargetRuntime.required_env`.

### 10.7 Cycle detection

`import_compose` reuses `capsule::engine::orchestration::startup_order_from_dependencies`
for DFS topological sort and cycle detection. The same function is used in
`to_orchestration_plan()` to produce `OrchestrationPlan.startup_order`.

### 10.8 Integration with PR 8 executor

`ComposeImportOutput::to_orchestration_plan()` converts the import output to
an `OrchestrationPlan` using `ResolvedServiceRuntime::Oci`. The caller must
supply `HashMap<String, OciImageResolution>` (resolved digests from the lock)
before passing to `execute_service_graph_with_provider`. Digest resolution is
not part of the importer.

### 10.9 install.sh importer

`install.sh` intent extractor remains deferred to PR 11.

---

## §11 PR 10 — CLI Wiring for Docker Compose Import → OCI Lock/Plan/Run

### 11.1 Goal

Wire the PR 9 pure Compose importer into the CLI so that a repo with a
`docker-compose.yml` can be imported, image refs resolved to lock digests,
converted to the existing multi-service OCI execution path, and run through
`PodmanProvider`.

### 11.2 Entry point

A new hidden/experimental flag `--oci-compose` is added to `ato run`:

```sh
ato run . --oci-compose
```

This path is guarded and does **not** change the default `ato run .` behavior.
Normal source runs are completely unaffected.

### 11.3 Dispatch model

In `execute_run_like_command` (dispatch/run.rs), an early-return check fires
before sandbox-mode flag processing and before the share-artifact path:

1. Detect compose candidate in `args.path` directory.
2. Import with `compose::import_compose` → `ComposeImportOutput`.
3. Surface hard errors as typed failures.
4. Convert to orchestration plan via `to_orchestration_plan()`.
5. Resolve image digests for all services via `resolve_images_for_compose`.
6. Require digests before any `PodmanProvider` execution (Required mode).
7. Execute via `execute_service_graph_with_provider` (same path as PR 8).

The testable core is `execute_compose_run_with_provider<P: OciProvider>` in
`oci_compose_runner.rs`, mirroring the pattern from `oci_multi_service.rs`.

### 11.4 Image digest resolution gate

`resolve_images_for_compose` calls `provider.resolve_image()` for each service
and returns `Err(OciImageResolutionRequired)` if any service has no digest.
The execution gate in `execute_service_graph_with_provider` additionally rejects
any start attempt with a missing image entry.

### 11.5 Diagnostics and receipt

The Compose runner surfaces:
- Selected compose file path in diagnostics.
- Importer warnings (unsupported features) in diagnostics/receipt.
- Resolved digests per service.
- Policy enforcement result at graph level.
- Redacted env/secret keys (keys matching PASSWORD, SECRET, TOKEN, KEY).

Not exposed:
- Secret values.
- Generated passwords.
- Container IDs or allocated host ports in identity.
- Global Compose `container_name` as runtime name.

### 11.6 Supported CLI wiring shapes

| Pattern | Notes |
|---------|-------|
| `ato run . --oci-compose` | Discovers compose file in current dir |
| `ato run ./myapp --oci-compose` | Discovers compose file in `./myapp` |

Future: `ato run github.com/org/repo --oci-compose` (after repo materialization is wired).

### 11.7 Tests (11 tests in `oci_compose_runner.rs`)

| Test | Description |
|------|-------------|
| `cli_compose_flag_discovers_compose_file` | Compose file is auto-detected from dir |
| `cli_compose_flag_imports_graph_without_docker_compose` | No shell-out to docker compose |
| `cli_compose_import_errors_are_typed` | Hard importer errors are typed |
| `cli_compose_warnings_are_reported` | Importer warnings are propagated |
| `cli_compose_requires_image_digest_before_execution` | Gate rejects missing digest |
| `cli_compose_resolves_all_service_images_into_lock` | All services get resolved digest |
| `cli_compose_executes_imported_graph_with_fake_provider` | Full path with FakeOciProvider |
| `cli_compose_does_not_use_legacy_bollard_path` | Bollard path is never invoked |
| `cli_compose_redacts_secret_like_env_values` | Secret keys are redacted in receipt |
| `blinko_style_compose_smoke_imports_and_executes_with_fake_provider` | Blinko smoke test |
| `normal_source_run_behavior_unchanged` | Source run not affected |

### 11.8 install.sh / docker-run importer

`install.sh` and `docker run` intent extractor are deferred to PR 11.
Compose CLI wiring is stable before adding an additional import surface.

---

## §12 PR 10.5 — Compose CLI Hardening: Failure-Path Tests, Diagnostics, Opt-in Real Podman Smoke

### 12.1 Goal

Harden the `--oci-compose` path added in PR 10:
- Typed failure-path coverage for all resolution/pull/policy error classes.
- Improved diagnostics: compose file path, service list, and per-service resolved digest are
  printed to the reporter before execution.
- An opt-in `#[ignore]` real Podman smoke test.
- A manual smoke verification doc at `docs/manual/oci-compose-podman-smoke.md`.

### 12.2 Diagnostics improvements

`execute_compose_run` now emits (via `reporter.notify`) before execution:

```
📋 Compose file: <path>
🔧 Services: <comma-separated service names>
✅ [<service>] Resolved: sha256:... (first 19 chars)
⚠️  compose: <warning>   (for each importer warning)
```

**Must NOT appear in diagnostics:**
- Secret values
- Raw `DATABASE_URL` if it contains a password
- Global `container_name` as a runtime name

### 12.3 FakeOciProvider extensions

Two new constructors added to `FakeOciProvider` for error injection:

| Constructor | Behavior |
|-------------|----------|
| `FakeOciProvider::with_resolve_error(err)` | `resolve_image()` returns the given error |
| `FakeOciProvider::with_pull_failure(err)` | `pull_image()` returns the given error |

### 12.4 Failure-path tests (6 new tests, total 17)

| Test | Coverage |
|------|----------|
| `image_resolve_unsupported_returns_resolution_required_error` | `Unsupported` variant → `oci_image_resolution_required` error |
| `image_resolve_generic_failure_is_propagated` | `Operation` variant → error propagates with context |
| `pull_failure_in_compose_graph_is_typed` | `pull_image` failure → typed error from executor |
| `strict_egress_gap_blocks_compose_execution` | `Strict` + non-empty `egress_allow` → `oci_execution_gate_failed` |
| `loose_policy_gap_allows_compose_execution` | `Loose` + `egress_allow` → execution succeeds |
| `real_podman_compose_smoke_minimal_two_service` | `#[ignore]` real Podman opt-in smoke |

### 12.5 Real Podman opt-in smoke test

The test `real_podman_compose_smoke_minimal_two_service` is marked `#[ignore]`
and guarded by `ATO_TEST_REAL_PODMAN=1`:

```sh
ATO_TEST_REAL_PODMAN=1 cargo test -p ato-cli real_podman -- --ignored --nocapture
```

- Uses `alpine:3.19` for both services to minimize pull time.
- `app` depends_on `db` and exits after `sleep 3`.
- `db` is a background sleeper (`sleep 30`).
- Accepts graceful skip if Podman is not available or not ready.

### 12.6 Lock persistence status

Image digest resolution in `resolve_images_for_compose` is **in-memory only**.
No lock file write path exists yet for Compose-imported services. This is
documented as diagnostic-only until a future PR adds lock persistence for
the Compose import path.

Execution is still gated on digest presence (the `images` map must be populated
before `execute_service_graph_with_provider` is called).

### 12.7 Known pre-existing blocker (unchanged)

`cargo test --workspace` triggers an interactive consent prompt. Always run
per-crate filters:

```sh
cargo test -p ato-cli oci_compose --lib
cargo test -p capsule compose_import --lib
```

### 12.8 Next: PR 11 (deferred)

`install.sh` and `docker run` intent extractor remain deferred to PR 11.
The Compose CLI wiring is stable before adding an additional import surface.

## §13 PR 10.6 — OCI Compose Lock Persistence (`ato.oci.lock.json`)

### 13.1 Goal

Persist OCI image digest resolutions for Compose-imported services to
`ato.oci.lock.json` so that reruns can replay from the locked digest instead of
re-resolving every time. This completes the lock persistence gap documented in
§12.6.

### 13.2 Lock file: `ato.oci.lock.json`

A new, separate file at `<project_root>/ato.oci.lock.json`. It is distinct from
`capsule.lock.json` (source capsule lock) and `ato.lock.json` (canonical
workspace lock) to avoid polluting those pipelines.

**Format**:

```json
{
  "version": 1,
  "import": {
    "kind": "compose",
    "source_path": "docker-compose.yml",
    "source_hash": "sha256:<hex>"
  },
  "images": {
    "<service-name>": {
      "declared_ref": "postgres:14",
      "resolved_digest": "sha256:<hex>",
      "platform": "linux/amd64",
      "provider_semantics": "podman-rootless-native-v1"
    }
  }
}
```

**Fields that are intentionally absent** (never written to the lock):

- `container_id`, `network_id`, `volume_id` — live state, belongs in session
- `host_port` — allocated at runtime, not part of execution identity
- secret values, `DATABASE_URL` contents

### 13.3 Replay behavior

On rerun (`ato run . --oci-compose`):

1. `ato.oci.lock.json` is loaded (parse errors are non-fatal: re-resolve).
2. For each service, `entry_is_fresh()` checks:
   - `source_hash` matches
   - `declared_ref` matches
   - `provider_semantics` (coarse label) matches
3. Fresh entries → reuse persisted digest without a provider round-trip (♻️).
4. Any drift → re-resolve via provider (✅) and write a fresh entry.
5. Updated lock is written before execution starts.
6. Execution is blocked until all services have a persisted digest.

### 13.4 Mutable tags

Mutable tags (`latest`, branch tags) are allowed only after digest resolution
has occurred. The resolved digest is written to the lock; subsequent reruns
replay it unless source drift or provider semantics change.

### 13.5 Digest-ref round-trips

If `declared_ref` is already `image@sha256:<digest>`, the lock reuses it
without unnecessary re-resolution when the source hash, ref, and provider
semantics are all unchanged.

### 13.6 Identity stability

`OciComposeLock::execution_identity_hash()` computes a SHA-256 over:

- `source_hash`
- Per-service `(name, declared_ref, resolved_digest, platform, provider_semantics)` in sorted order

Changes that **affect** identity: resolved digest change, platform change,
provider semantics label change, compose source hash change.

Changes that **do not affect** identity: allocated host port, container id,
network id, volume id, secret values.

### 13.7 Provider semantics label

Produced by `OciProviderSemantics::coarse_label()` in the format
`"<kind>-<mode>-<substrate>-v1"` (e.g. `"podman-rootless-native-v1"`).
Only enum variants are used — minor version changes in Podman do not
invalidate lock entries.

A label change (e.g. rootless → rootful, or Podman → future
`AtoNativeOciProvider`) changes execution identity. The caller may choose to
re-resolve or mark drift; currently drift forces re-resolve.

### 13.8 New module

`crates/capsule/src/contract/oci_compose_lock.rs` — `OciComposeLock`,
`OciImageLockEntry`, `OciImportMeta`, `OciLockError` (6 typed variants),
`load_from_dir`, `write_to_dir`, `compute_compose_source_hash`,
`parse_platform_str`, `OciComposeLock::execution_identity_hash`,
`OciComposeLock::entry_is_fresh`.

### 13.9 New function in runner

`resolve_images_with_lock_replay` in `oci_compose_runner.rs` encapsulates the
replay / re-resolve logic. `execute_compose_run` was updated to:

1. Compute `source_hash` from compose file content.
2. Load existing lock (`ato.oci.lock.json`).
3. Call `resolve_images_with_lock_replay` (per-service reuse or fresh resolve).
4. Write updated lock **before** delegating to the executor.

`resolve_images_for_compose` is kept as-is for existing tests that do not
require lock replay.

### 13.10 Tests

**`capsule` (18 tests in `oci_compose_lock.rs`)**:

- `lock_serializes_and_deserializes`
- `lock_roundtrips_via_dir`
- `load_from_dir_returns_none_when_file_absent`
- `lock_write_failure_returns_typed_error_from_write`
- `lock_parse_failure_returns_typed_error_from_load`
- `compute_compose_source_hash_is_deterministic`
- `compute_compose_source_hash_differs_on_changed_content`
- `parse_platform_str_roundtrips`
- `parse_platform_str_roundtrips_with_variant`
- `entry_is_fresh_matches_correct_inputs`
- `entry_is_fresh_rejects_source_hash_drift`
- `entry_is_fresh_rejects_declared_ref_drift`
- `entry_is_fresh_rejects_provider_semantics_drift`
- `resolved_digest_drift_changes_execution_identity`
- `allocated_host_port_does_not_change_execution_identity`
- `container_id_does_not_change_execution_identity`
- `provider_semantics_drift_changes_execution_identity`
- `secret_values_are_not_in_identity`

**`ato-cli` (7 new tests in `oci_compose_runner.rs`, total 24)**:

- `compose_run_writes_oci_image_resolutions_to_lock`
- `compose_run_reuses_existing_lock_resolution`
- `compose_source_hash_drift_triggers_fresh_resolution`
- `mutable_tag_without_persisted_digest_triggers_resolution`
- `digest_ref_round_trips_without_lock_churn`
- `secret_values_are_not_persisted_to_lock`
- `blinko_style_compose_lock_replay_with_fake_provider`

### 13.11 Known limitations before PR 11

- Real Podman smoke for lock persistence remains opt-in only
  (`ATO_TEST_REAL_PODMAN=1`).
- `install.sh` / `docker run` intent extractor remains PR 11.
- Lock file is written only on the `--oci-compose` and `--oci-install-sh`
  paths; standard `ato run` is unaffected.
- Starting with PR 241, OCI resolution facts are written to both
  `ato.lock.json` (resolution.oci_images / resolution.oci_imports) and the
  sidecar `ato.oci.lock.json`. The dual-write preserves backward compatibility;
  Phase 2 will remove the sidecar write.

### 13.12 Known pre-existing blocker (unchanged)

`cargo test --workspace` triggers an interactive consent prompt. Always run:

```sh
cargo test -p ato-cli oci_compose --lib
cargo test -p capsule oci_compose_lock --lib
cargo test -p capsule compose_import --lib
```

---

## §14 PR 11 — install.sh / docker run intent extractor

### 14.1 Motivation

Docker-only apps are typically distributed through:

1. `docker-compose.yml` (handled by `--oci-compose`, PR 8–10.6)
2. An `install.sh` / `setup.sh` that contains `docker run` commands
3. A `README.md` with manual `docker run` instructions

PR 11 covers case 2: extract `docker run` command **intent** from install
scripts without executing the script.  The install script is treated as
a static document; no shell evaluation occurs.

### 14.2 Supported script subset

Only the following patterns are extracted:

```
docker network create <name>
docker run -d [flags] IMAGE [CMD]
```

Line continuations (`\` + newline) are joined before tokenisation.

**Supported `docker run` flags:**

| Flag | Handling |
|------|----------|
| `--name <name>` | Logical service label candidate (sanitised) |
| `--network <name>` | Service association metadata only |
| `-e KEY=VALUE` / `--env KEY=VALUE` | Env variable; secret-like keys are classified |
| `-p HOST:CONTAINER` / `--publish HOST:CONTAINER` | Port mapping |
| `-p CONTAINER` (no host) | Container-only port; host auto-assigned |
| `-v SOURCE:TARGET` / `--volume SOURCE:TARGET` | Volume or bind mount (rules below) |
| `--restart <policy>` | Parsed but **ignored** (see §14.5) |
| `IMAGE` (final positional arg) | Declared image ref |

**Unsupported — rejected with typed error or warning:**

| Pattern | Outcome |
|---------|---------|
| `--privileged` | `PrivilegedRejected` error, extraction stops |
| `--network host` | `HostNetworkRejected` error, extraction stops |
| Absolute bind mounts (`-v /host/path:…`) | `AbsoluteBindMountRejected` error |
| Relative bind mounts (`-v ./path:…`) | Allowed with warning; mapped to `ProjectRootBind` |
| `--cap-add`, `--cap-drop` | Unsupported, logged as warning |
| `--device`, `--userns`, `--pid`, `--ipc` | Unsupported, logged as warning |
| `docker build` | Not extracted |
| `docker compose` invocation | Not extracted |
| Shell variable substitution (`$VAR`, `${VAR}`) in mount paths | Rejected |
| Shell variable substitution in env values | `RequiredExternal` ref with warning |
| Prompts / interactive `read` blocks | Not executed; static commands before/after extracted |
| Command substitution (`$(…)`) | Not evaluated; surrounding docker run skipped |

### 14.3 Mapping rules

| Script pattern | Ato OCI model |
|----------------|---------------|
| `docker network create <name>` | Source metadata only; actual network is session-scoped |
| `--name <name>` | Logical service label candidate; sanitised to `[a-z0-9-]`; NOT the runtime container name |
| `--network <net>` | Service-to-service association clue; not preserved as fixed network name |
| `-p HOST:CONTAINER` | `container_port`; host port is auto-assigned at runtime |
| Named volume (`-v pgdata:/var/lib/…`) | `StateBindingKind::Named` |
| Relative bind (`-v ./data:/app/…`) | `StateBindingKind::ProjectRootBind` + warning |
| `DATABASE_URL` containing `@<service>:` | Service alias rewritten to logical label; infers `depends_on` |
| `--restart always` | Ignored; Ato session owns lifecycle |

### 14.4 Secret handling

Env keys matching the following patterns are classified as secret-like:

`PASSWORD`, `PASSWD`, `SECRET`, `TOKEN`, `KEY`, `CREDENTIAL`

Rules:

- **Secret values must not enter the lock file.**
- **Secret values must not enter execution identity.**
- Unsafe literal defaults (e.g. `mysecretpassword`, `my_ultra_secure_nextauth_secret`)
  emit a warning and are converted to generated secret requirements.
- `DATABASE_URL` values that embed a password are rewritten to use a service
  alias + generated/redacted password reference when the service alias is
  unambiguous.  If ambiguous, the env is marked as `RequiredExternal`.

For Blinko-style install.sh:

```
POSTGRES_PASSWORD=mysecretpassword
  → generated secret requirement (POSTGRES_PASSWORD)

NEXTAUTH_SECRET=my_ultra_secure_nextauth_secret
  → generated secret requirement (NEXTAUTH_SECRET)

DATABASE_URL=postgresql://postgres:mysecretpassword@blinko-postgres:5432/blinko
  → postgresql://postgres:<POSTGRES_PASSWORD>@db:5432/blinko  (alias rewritten)
```

### 14.5 Lifecycle ownership — `--restart` ignored

`--restart always` (and any other restart policy) is deliberately ignored.

**Reason**: Ato session owns container lifecycle. Containers are started and
stopped as a unit with the session.  A persistent restart policy would create
containers that outlive the Ato session, undermining the `--stop` / cleanup
semantics established in PR 7.

A warning is emitted when `--restart` is encountered so the user knows their
script's intent was not translated.

### 14.6 Lock persistence model (reuses PR 10.6, dual-write in PR 241)

PR 11 reuses `OciComposeLock` from PR 10.6 for the sidecar format. Starting
with PR 241, OCI resolution facts are also written to the main lock
(`ato.lock.json` under `resolution.oci_images` and `resolution.oci_imports`),
while the sidecar continues to be written for backward compatibility.

The sidecar `OciComposeLock` format — only `import.kind` differs:

```json
{
  "oci": {
    "images": {
      "postgres": {
        "declared_ref": "postgres:14",
        "resolved_digest": "sha256:...",
        "platform": "linux/arm64",
        "provider_semantics": "podman-rootless-v1"
      },
      "app": {
        "declared_ref": "blinkospace/blinko:latest",
        "resolved_digest": "sha256:...",
        "platform": "linux/arm64",
        "provider_semantics": "podman-rootless-v1"
      }
    },
    "import": {
      "kind": "docker-run-script",
      "source_path": "install.sh",
      "source_hash": "sha256:..."
    }
  }
}
```

**Lock replay rules (identical to §13.x):**

| Condition | Action |
|-----------|--------|
| `source_hash + declared_ref + platform + provider_semantics` all match | Reuse lock digest |
| `source_hash` changed | `OciLockComposeDrift` error — refresh required |
| `declared_ref` changed | Re-resolve |
| `platform` changed | Re-resolve |
| `provider_semantics` changed | Re-resolve |
| `declared_ref` is already `image@sha256:…` | Preserved as-is; no re-resolution needed |

**Mutable tags**: must not proceed to pull/start without a persisted digest.
If no lock entry exists, resolution runs and the lock is written before
any pull or start.

### 14.7 Why install.sh is not executed

Executing arbitrary shell scripts is a significant security surface.  The
subset of `docker run` commands that matter for Ato can be extracted purely
statically.  Any script patterns that cannot be safely extracted statically
(conditionals, command substitution, network calls) are simply not extracted
— the user gets a warning listing unsupported patterns and can annotate their
intent in a `capsule.toml` instead.

### 14.8 CLI wiring

```sh
ato run . --oci-install-sh
```

This flag is hidden (not shown in `--help`).  It bypasses the normal capsule
resolution pipeline and:

1. Scans the project root for a candidate script:
   `install.sh`, `setup.sh`, `start.sh`, `run.sh`, `deploy.sh`,
   `docker-install.sh`, `docker-setup.sh`, `docker-run.sh`
2. Parses with `DockerRunScriptImporter` (pure, no I/O)
3. Emits warnings for ignored/unsupported patterns
4. Resolves or replays OCI image digests via `OciComposeLock`
5. Writes/updates both `ato.lock.json` (resolution.oci_images/oci_imports)
   and `ato.oci.lock.json` for backward compatibility
6. Executes through the PR 8 multi-service Podman path

No legacy Bollard route.  `--oci-compose` and `--oci-install-sh` are
independent flags; using both is not tested and not recommended.

### 14.9 Blinko install.sh mapping

Blinko's install.sh produces two services:

| Script `--name` | Logical label | Role |
|-----------------|---------------|------|
| `blinko-postgres` | `blinko-postgres` | Postgres database |
| `blinko-website` | `blinko-website` | Blinko web app |

Dependencies inferred from:

- `DATABASE_URL` host `blinko-postgres` → `blinko-website` depends_on
  `blinko-postgres`
- Shared `--network blinko-network` links both services

Main published port: `1111` (blinko-website).
Postgres port remains internal.

### 14.10 Tests added (PR 11)

**`capsule` (18 tests in `docker_run_script.rs`):**

- `parses_simple_docker_run_single_service`
- `parses_blinko_install_sh_two_services`
- `docker_network_name_is_source_metadata_only`
- `docker_name_becomes_logical_label_not_runtime_name`
- `restart_always_is_ignored_with_warning`
- `absolute_bind_mount_is_rejected`
- `prompt_control_flow_is_not_executed`
- `unsupported_privileged_is_rejected`
- `host_network_is_rejected`
- `port_mapping_uses_container_port_with_auto_host_port`
- `database_url_service_name_is_rewritten_to_alias`
- `secret_like_env_values_are_redacted`
- `unsafe_default_password_warns_or_generates_secret`
- `install_sh_source_hash_written_to_oci_lock`
- `rerun_reuses_install_sh_lock_entries`
- `source_hash_drift_requires_refresh_or_reresolve`
- `imported_install_sh_graph_executes_with_fake_provider`
- `install_sh_path_does_not_use_legacy_bollard`

**`ato-cli` (6 tests in `install_sh_runner.rs`):**

- `rerun_reuses_install_sh_lock_entries`
- `source_hash_drift_requires_refresh_or_reresolve`
- `install_sh_source_hash_written_to_oci_lock`
- `blinko_style_install_sh_executes_with_fake_provider`
- `install_sh_path_does_not_use_legacy_bollard`
- `secret_values_are_not_persisted_to_lock`

### 14.11 Known limitations after PR 11

- Real Podman smoke for install.sh path remains opt-in only
  (`ATO_TEST_REAL_PODMAN=1`).
- Scripts with complex conditional logic (e.g., `if [ -z "$POSTGRES_PASSWORD" ]`)
  are parsed but the conditional branches are not evaluated; only the static
  `docker run` commands are extracted.
- `docker run` commands using `--env-file` are not yet supported.
- Multiple compose files or mixed compose + install.sh in one project is not
  tested.
- PR 12 target: `docker run` single-command import from README-style snippets.

---

## §15 PR 11.5 — Blinko Smoke, Invariant Hardening, and Merge Readiness

### 15.1 Invariants strengthened

PR 11.5 adds and explicitly documents the following invariants:

1. **Script is never executed** — `DockerRunScriptImporter` operates purely on
   the script text as a string. No shell subprocess is created. Non-docker
   commands (`apt-get`, `curl`, `rm`, etc.) are silently skipped.

2. **`--name` is logical label, not runtime name** — The `--name` value from a
   `docker run` command is used to derive the logical service label inside
   `OrchestrationPlan`. The actual runtime container name is `ato-<project>-<label>-<session_sfx>`,
   computed at execution time by `service_container_name()` in `oci_multi_service.rs`.
   The two are never the same in production.

3. **Raw secret values never reach the lock or diagnostics** — All env keys
   classified as secret-like (contains PASSWORD, PASSWD, SECRET, TOKEN, KEY,
   CREDENTIAL) have their values replaced with a redacted placeholder before
   any log/diagnostic output is produced. The `OciComposeLock` model does not
   have any field for env values; secrets therefore cannot appear in
   `ato.oci.lock.json` by design.

4. **DATABASE_URL embedded passwords are redacted** — When a `DATABASE_URL`
   embeds a password that is also flagged as a secret-like env, the password
   portion is replaced with a `{generated:<KEY>}` placeholder and a warning
   is emitted. The redacted URL (with service alias substituted) may appear in
   diagnostics; the raw password does not.

5. **Session-scoped runtime names** — Verified by the existing
   `service_container_name_is_session_scoped` test in `oci_multi_service.rs`
   and the new `docker_name_becomes_logical_label_not_runtime_container_name`
   test in `install_sh_runner.rs`.

### 15.2 Lock file naming and future migration

#### Current status

The OCI import lock is stored in `ato.oci.lock.json` as an **experimental
sidecar lock file**, separate from the primary `ato.lock.json` manifest lock.

Rationale for separation:
- The OCI lock schema is still evolving (PR 10.6–11.5).
- It conflates `import` metadata (compose / install.sh source hash) with
  `image` resolution data in one JSON object. The primary lock uses a
  different data model.
- Keeping them separate avoids breaking existing `ato.lock.json` parsers
  while the OCI model stabilises.

#### Fields stored in `ato.oci.lock.json` (current)

```json
{
  "version": 1,
  "import": {
    "kind": "compose" | "docker-run-script",
    "source_path": "docker-compose.yml" | "install.sh",
    "source_hash": "sha256:..."
  },
  "images": {
    "<service-label>": {
      "declared_ref": "<image:tag>",
      "resolved_digest": "sha256:...",
      "platform": "linux/arm64",
      "provider_semantics": "podman-rootless-v1"
    }
  }
}
```

Fields intentionally absent: container id, network id, volume id, allocated
host port, secret values, Podman machine id.

#### Migration status (PR 241 / Phase 2)

**Current (Phase 1 complete):** OCI resolution facts are written to both
`ato.lock.json` (resolution.oci_images / resolution.oci_imports) and the
sidecar `ato.oci.lock.json`. The read path prefers the main lock and falls
back to the sidecar. This dual-write preserves backward compatibility.

**Phase 2 (planned):** Remove sidecar write. Write to `ato.lock.json` only.
Emit a warning when a stale sidecar is present.

**Phase 3 (planned):** Remove sidecar read support entirely. `ato.oci.lock.json`
is fully deprecated and consumers should not parse it directly.

**PR 11 reuse note**: install.sh and compose importers share the same
`OciComposeLock` Rust type. The `import.kind` field (`compose` vs
`docker-run-script`) distinguishes them. No separate lock format was created
for the install.sh importer.

### 15.3 Policy and mount safety table

The following table documents how each Docker-run flag is handled across the
three OCI import paths (explicit capsule, `--oci-compose`, `--oci-install-sh`):

| Flag / feature | Compose | install.sh | Explicit capsule | Notes |
|---|---|---|---|---|
| Absolute bind mount (`-v /host:/ctr`) | Rejected | Rejected | Rejected | Security hard-reject |
| Relative bind mount (`-v ./data:/ctr`) | Rejected or project-scoped | Rejected or project-scoped | Rejected | Warning emitted if project-scoped |
| Named volume (`-v pg_data:/data`) | Allowed → state binding | Allowed → state binding | Allowed via `[state]` | Persisted as Ato state binding |
| `--privileged` | Rejected | Rejected | Rejected | Hard reject; typed error |
| `--network host` | Rejected | Rejected | Rejected | Hard reject; typed error |
| `--restart <policy>` | Ignored with warning | Ignored with warning | N/A | Ato session owns lifecycle |
| `--cap-add / --cap-drop` | Rejected | Rejected | Rejected | Not in current scope |
| `--device` | Rejected | Rejected | Rejected | Not in current scope |
| `--userns / --pid host` | Rejected | Rejected | Rejected | Not in current scope |
| Strict + unsupported egress | Blocked | Blocked | Blocked | Via `OciPolicyMode::Strict` |
| Loose / Off policy | Allowed with diagnostic | Allowed with diagnostic | Allowed with diagnostic | For opt-in experiments |

### 15.4 Diagnostics emitted by `--oci-install-sh`

After PR 11.5, the `--oci-install-sh` path prints:

```
📋 Install script: /path/to/install.sh
🔑 Source hash: sha256:...
🔗 Extracted networks: blinko-network        ← if any
🔧 Services: blinko-postgres, blinko-website
   blinko-postgres → image: postgres:14
   blinko-website  → image: blinkospace/blinko:latest
⚠️  install.sh: ...                           ← any warnings (redacted secrets, --restart, etc.)
ℹ️  install.sh (unsupported, skipped): ...    ← unsupported features
♻️  [blinko-postgres] Reusing lock: postgres:14 → sha256:abc123...  ← lock replay
✅ [blinko-website] Resolved: sha256:def456...                       ← fresh resolve
🔒 Lock written: ato.oci.lock.json
🔒 Main lock updated with OCI facts                                 ← main lock write (PR 241)
🌐 OCI service 'blinko-website' available at http://127.0.0.1:<port>/
```

Never printed:
- Raw `POSTGRES_PASSWORD` value
- Raw `NEXTAUTH_SECRET` value
- Raw `DATABASE_URL` with embedded password
- Generated secret values
- Podman machine id or internal container id

### 15.5 Tests added (PR 11.5)

**`ato-cli` (new tests in `install_sh_runner.rs`):**

- `non_docker_commands_in_script_are_ignored_not_executed`
- `docker_name_becomes_logical_label_not_runtime_container_name`
- `database_url_embedded_password_not_in_lock_json`
- `blinko_style_graph_has_correct_startup_order`
- `real_podman_install_sh_smoke_single_service` (opt-in `#[ignore]`)

**Fixture files added:**

- `crates/ato-cli/tests/fixtures/install_sh/blinko_style.sh`
- `crates/ato-cli/tests/fixtures/install_sh/lightweight_two_service.sh`
