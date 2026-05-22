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
