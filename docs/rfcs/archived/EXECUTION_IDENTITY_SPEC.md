---
title: "Execution Identity Spec: Snapshot-derived execution_id (Superseded)"
status: archived       # superseded by Capsule v1 execution model
date: 2026-07-13
author: "@egamikohsuke"
ssot:
  - "crates/snapshot/src/manifest.rs"
related:
  - "../draft/CAPSULE_V1_EXECUTION_MODEL_SPEC.md"
  - "../draft/GITHUB_CAPSULE_REQUEST_PIPELINE.md"
  - "../draft/A1_SOURCE_TREE_PROFILE.md"
  - "../accepted/ADR-002-signature-format-jcs.md"
  - "../draft/HASH_AND_PROVENANCE_POLICY.md"
---

# Execution Identity Spec

> **Superseded:** This Snapshot-derived post-seal identity model is retained for
> design history only. The Capsule v1 model is
> [`CAPSULE_V1_EXECUTION_MODEL_SPEC.md`](../draft/CAPSULE_V1_EXECUTION_MODEL_SPEC.md),
> where Execution Identity identifies the resolved launch contract and Snapshot
> is a subordinate cache.

## 1. Overview

`execution_id` is the reproducible identity of the **actual sealed execution** —
not of the build inputs. It is a digest over the *post-seal* artifact: the
sealed layer CAS IDs (rootfs / runtime / app) plus the concrete launch spec
(entrypoint, argv, cwd, effective environment, filesystem view) and the
policy/runner-class facets. Two artifacts with the same `execution_id` boot the
same bytes the same way; any change to the sealed output or the launch spec
changes it.

Identity is taken **post-seal on purpose**. Lane B builds (docker-import) are
**not input-deterministic**: a Dockerfile pulls from mutable package sources, so
the same repo+commit can produce different bytes on two builds. The recipe and
source hashes are therefore recorded as *provenance*, but the thing
`execution_id` actually pins is the sealed output that QA verified.

This is delivered as an **extension of the existing `ReadyStateManifest`**
(`crates/snapshot/src/manifest.rs`, schema `ato.ready-state/v1`), which already
declares an `execution_id: Option<String>` field, carries the sealed layers as
`BlobManifest` CAS refs, and computes its own structural id via JCS + BLAKE3.
**No new receipt type is introduced, and the `ato.ready-state/v1` schema tag is
not bumped** — the facet set carries its own version (§3.1).

## 2. Scope

### In scope

- The identity facets folded into `execution_id`.
- The new `ReadyStateManifest` fields that carry those facets.
- The canonicalization and hash that produce `execution_id`.
- The teardown-receipt requirement for publication.

### Out of scope

- RunnerClass sentinel resolution that supplies `rootfs_base_id` /
  `guest_kernel_id` — a **prerequisite tracked in a separate PR** (see §3.4).
- The source-tree hash algorithm
  ([A1_SOURCE_TREE_PROFILE.md](../draft/A1_SOURCE_TREE_PROFILE.md)).

## 3. Design

### 3.1 New manifest fields (facets) and the facet-set version

Added to `ReadyStateManifest` (additive, default-safe so artifacts sealed before
this change still deserialize). Because the fields are additive and their meaning
is governed by an independent version, **the `ato.ready-state/v1` schema tag is
not bumped**: a new field `execution_identity_schema` (e.g. `ato.exec-id/v1`)
governs the facet set below, so facets can evolve without touching the
ready-state wire schema.

**Sealed-output facets — the ground truth `execution_id` pins.** Sourced from the
sealed artifact *after* build, so they are correct even for non-deterministic
Lane B builds. The layer hashes come directly from the `ReadyStateLayers`
`BlobManifest` CAS refs already on the manifest:

| Field | Source / meaning |
|-------|------------------|
| `sealed_rootfs_hash` | CAS id of the sealed `rootfs` layer (`ReadyStateLayers.rootfs` `BlobManifest`). |
| `runtime_layer_hash` | CAS id of the sealed `runtime` layer (`ReadyStateLayers.runtime` `BlobManifest`). |
| `app_layer_hash` | CAS id of the sealed `app` layer (`ReadyStateLayers.app` `BlobManifest`). |
| `entrypoint` | The resolved process entrypoint the artifact launches. |
| `argv` | The concrete argument vector. |
| `cwd` | The working directory at launch. |
| `effective_environment_hash` | Digest of the effective (secret-free) environment presented at launch. |
| `filesystem_view_hash` | Digest of the mounted filesystem view (layer composition + mount topology). |

**Provenance facets — recorded, and folded in, but not the determinism anchor.**
These describe *what was asked for*; for Lane B they do not by themselves pin the
bytes:

| Field | Meaning |
|-------|---------|
| `source_tree_hash` | The `materialized_source_tree_hash` (A1v2 `sha256`) of the source that produced this execution. |
| `recipe_hash` | Identity of the generated `capsule.toml` recipe (canonicalized manifest). |
| `dependency_derivation_hash` | The A1 `derivation_hash` of the dependency *recipe* (lockfile/ecosystem/policy inputs). |
| `dependency_output_hash` | The A1 `blob_hash` of the dependency *output* (the frozen install). |
| `sandbox_policy_hash` | Identity of the effective sandbox/isolation policy. |
| `network_policy_hash` | Identity of the effective network policy. |
| `qa_contract_hash` | Identity of the QA contract the candidate was verified against. |

`runner_class_id` already exists on the manifest and continues to carry the
restore-compatibility class; the rootfs/kernel facets (§3.4) come from it.

### 3.2 Empty-derivation constants, never NULL

A **static-web** capsule (Lane A) has no dependency install. It must **not** set
`dependency_derivation_hash` / `dependency_output_hash` to NULL — a NULL would
make two different executions collide on "unknown". Instead it uses **defined
empty-derivation constants**: a canonical `derivation_hash` and `blob_hash` for
"the empty derivation / empty output". This keeps `execution_id` total: every
facet always has a definite value.

### 3.3 Computing `execution_id`

```text
execution_id = "blake3:" + hex(blake3(JCS(execution_identity_facets)))
```

- The facets are collected **post-seal** (§3.1): the sealed-output facets are
  read from the finished artifact's `ReadyStateLayers` CAS refs and launch spec,
  so `execution_id` identifies what actually booted, not what was requested.
- The facet set is serialized with a **single pinned JCS implementation**
  (`serde_jcs`, RFC 8785 — the same canonicalization ADR-002 adopted and the
  same one `ReadyStateManifest::id()` already uses). One impl, **pinned with
  test vectors**, so CLI and any other producer agree byte-for-byte.
- The fold uses **BLAKE3**, placing `execution_id` in the structural-id family
  (like `ReadyStateManifest::id()`), consistent with the hash-role split:
  `sha256` facets are *identity* inputs; the `blake3` fold is the *structural
  id* of the facet set. See
  [HASH_AND_PROVENANCE_POLICY.md](../draft/HASH_AND_PROVENANCE_POLICY.md) §2.
- The facet set is a fixed, ordered schema governed by
  **`execution_identity_schema`** (independent of `ato.ready-state/v1`); JCS
  makes key order irrelevant, but the version pins which facets participate so a
  facet cannot be silently added or dropped, and the facet set can evolve without
  a ready-state schema bump.

### 3.4 Rootfs/kernel identity (prerequisite — resolved 2026-07-13)

`rootfs_base_id` and `guest_kernel_id` are facets of `execution_id` and are
resolved from the artifact's `RunnerClass`. The scope investigation found the
original "all-sentinel" premise wrong: the production builder + `runner serve`
path already resolves real `snapshot_format` / `vmm_version` /
`guest_kernel_id` via `FirecrackerBackend::runner_facts()`
(`crates/snapshot/src/firecracker.rs:304`), and the CLI path now delegates to
the same backend resolver (PR #1058) instead of the sentinel-bearing
`from_host()` probe. Two facets remain intentionally unresolved —
`cpu_template` (a deliberate snapshot flag-day if ever enabled) and
`rootfs_base_id` (`"unset"` by design under the per-capsule full-rootfs layer
model). The pipeline treats a runner-class change as a potential identity
change (pipeline spec §8, item 3).

### 3.5 Teardown receipt required for publication

A candidate may only be **published** if its `candidate_qa` job produced a
**teardown receipt** — proof the QA session booted to readiness and was torn
down cleanly. The teardown receipt is a publication gate, not a new artifact
type: it is part of the QA job's receipt output. No teardown receipt → the
candidate cannot leave `publish_ready`.

## 4. Interface

`execution_id` is written back to the candidate row and stamped into the sealed
`ReadyStateManifest`, so a published capsule's runtime identity is discoverable
and reproducible from its artifact alone.

## 5. Security

- Binding the **sealed** layer CAS ids + launch spec + policy + runner class into
  one digest means a published capsule's execution cannot be silently altered
  without changing `execution_id` — and because identity is post-seal, a
  non-deterministic Lane B rebuild that produces different bytes gets a different
  `execution_id` rather than masquerading as the verified one.
- The no-secret invariant of `ReadyStateManifest` is unchanged: the sealed
  layers carry no secret, so a QA-sealed artifact is reusable across hosts of the
  same `runner_class_id`.

## 6. Known limitations

- Runner-class kernel identity is real in the builder/runner path
  (`runner_facts()`); `cpu_template` and `rootfs_base_id` remain unset by
  deliberate decision (§3.4), so the runner-class facets are exactly as precise
  as those decisions allow.
- Whether a runner-class derivation change should force re-verification of
  existing snapshots is unresolved (pipeline spec §8, item 3).

## References

- `crates/snapshot/src/manifest.rs:19-63` — `ReadyStateManifest`, `READY_STATE_SCHEMA`, existing `execution_id`/`runner_class_id` fields.
- `crates/snapshot/src/manifest.rs:112-130` — `ReadyStateLayers` (`rootfs`/`runtime`/`app` as `BlobManifest` CAS refs — the source of the sealed-output facets).
- `crates/snapshot/src/manifest.rs:97-104` — `ReadyStateManifest::id()` (existing JCS + BLAKE3 structural id).
- [../accepted/ADR-002-signature-format-jcs.md](../accepted/ADR-002-signature-format-jcs.md) — JCS (RFC 8785) canonicalization.
- [A1_SOURCE_TREE_PROFILE.md](../draft/A1_SOURCE_TREE_PROFILE.md) —
  `source_tree_hash` (`materialized_source_tree_hash`).
