---
title: "OCI Provider, Lock, and Identity Spec"
status: draft
date: 2026-05-22
author: "@Koh0920"
ssot:
  - "crates/capsule-core/src/foundation/types/oci.rs"
  - "crates/capsule-core/src/contract/lock_runtime.rs"
  - "crates/capsule-core/src/engine/execution_identity/mod.rs"
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
PR 9: compose/docker-run/install.sh importers
PR 10: Desktop UX
```

## 7. References

- `docs/rfcs/accepted/CAPSULE_SPEC.md` - existing `runtime = "oci"` surface.
- `crates/capsule-core/src/routing/router/services.rs` - service graph already
  supports OCI service constraints.
- `crates/capsule-core/src/engine/runtime/oci.rs` - current Docker-compatible
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
`capsule-core::routing::importer::compose`. The importer converts a
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

`import_compose` reuses `capsule_core::engine::orchestration::startup_order_from_dependencies`
for DFS topological sort and cycle detection. The same function is used in
`to_orchestration_plan()` to produce `OrchestrationPlan.startup_order`.

### 10.8 Integration with PR 8 executor

`ComposeImportOutput::to_orchestration_plan()` converts the import output to
an `OrchestrationPlan` using `ResolvedServiceRuntime::Oci`. The caller must
supply `HashMap<String, OciImageResolution>` (resolved digests from the lock)
before passing to `execute_service_graph_with_provider`. Digest resolution is
not part of the importer.

### 10.9 install.sh importer

`install.sh` intent extractor remains deferred to PR 10.
