---
title: "Execution Identity Spec: execution_id via ReadyStateManifest extension"
status: draft          # draft | accepted | archived
date: 2026-07-13
author: "@egamikohsuke"
ssot:
  - "crates/snapshot/src/manifest.rs"
related:
  - "GITHUB_CAPSULE_REQUEST_PIPELINE.md"
  - "A1_SOURCE_TREE_PROFILE.md"
  - "../accepted/ADR-002-signature-format-jcs.md"
  - "HASH_AND_PROVENANCE_POLICY.md"
---

# Execution Identity Spec

## 1. Overview

`execution_id` is the reproducible identity of *how a capsule runs*: it binds
the source, the recipe, the resolved dependencies, the sandbox and network
policy, the QA contract, and the runner-class rootfs/kernel into one digest. Two
builds with the same `execution_id` are the same execution; a change to any bound
facet changes it.

This is delivered as an **extension of the existing `ReadyStateManifest`**
(`crates/snapshot/src/manifest.rs`, schema `ato.ready-state/v1`), which already
declares an `execution_id: Option<String>` field and computes its own
structural id via JCS + BLAKE3. **No new receipt type is introduced.**

## 2. Scope

### In scope

- The identity facets folded into `execution_id`.
- The new `ReadyStateManifest` fields that carry those facets.
- The canonicalization and hash that produce `execution_id`.
- The teardown-receipt requirement for publication.

### Out of scope

- RunnerClass sentinel resolution that supplies `rootfs_base_id` /
  `guest_kernel_id` — a **prerequisite tracked in a separate PR** (see §3.4).
- The source-tree hash algorithm ([A1_SOURCE_TREE_PROFILE.md](A1_SOURCE_TREE_PROFILE.md)).

## 3. Design

### 3.1 New manifest fields (facets)

Added to `ReadyStateManifest` (additive, default-safe so artifacts sealed before
this change still deserialize):

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

- The facet set is serialized with a **single pinned JCS implementation**
  (`serde_jcs`, RFC 8785 — the same canonicalization ADR-002 adopted and the
  same one `ReadyStateManifest::id()` already uses). One impl, **pinned with
  test vectors**, so CLI and any other producer agree byte-for-byte.
- The fold uses **BLAKE3**, placing `execution_id` in the structural-id family
  (like `ReadyStateManifest::id()`), consistent with the hash-role split:
  `sha256` facets are *identity* inputs; the `blake3` fold is the *structural
  id* of the facet set. See [HASH_AND_PROVENANCE_POLICY.md](HASH_AND_PROVENANCE_POLICY.md) §2.
- The facet set is a fixed, ordered schema; JCS makes key order irrelevant, but
  the field set is versioned with the manifest schema so a facet cannot be
  silently added or dropped.

### 3.4 Rootfs/kernel identity (prerequisite)

`rootfs_base_id` and `guest_kernel_id` are facets of `execution_id` and are
resolved from the artifact's `RunnerClass`. Today those come from a sentinel;
**RunnerClass sentinel resolution is a separate prerequisite PR** and is out of
scope here. Until it lands, `execution_id` is computed with the runner-class
facets from whatever the RunnerClass resolves to; the pipeline treats a
runner-class change as a potential identity change (open question — pipeline
spec §8).

### 3.5 Teardown receipt required for publication

A candidate may only be **published** if its `candidate_qa` job produced a
**teardown receipt** — proof the QA session booted to readiness and was torn
down cleanly. The teardown receipt is a publication gate, not a new artifact
type: it is part of the QA job's receipt output. No teardown receipt → the
submission cannot leave `publish_ready`.

## 4. Interface

`execution_id` is written back to the submission row and stamped into the sealed
`ReadyStateManifest`, so a published capsule's runtime identity is discoverable
and reproducible from its artifact alone.

## 5. Security

- Binding source + recipe + dependency + policy + runner class into one digest
  means a published capsule's execution cannot be silently altered without
  changing `execution_id`.
- The no-secret invariant of `ReadyStateManifest` is unchanged: the sealed
  layers carry no secret, so a QA-sealed artifact is reusable across hosts of the
  same `runner_class_id`.

## 6. Known limitations

- Runner-class rootfs/kernel identity depends on the sentinel-resolution
  prerequisite; until then the runner-class facets are as precise as the
  sentinel allows.
- Whether a runner-class derivation change should force re-verification of
  existing snapshots is unresolved (pipeline spec §8, item 3).

## References

- `crates/snapshot/src/manifest.rs:19-63` — `ReadyStateManifest`, `READY_STATE_SCHEMA`, existing `execution_id`/`runner_class_id` fields.
- `crates/snapshot/src/manifest.rs:97-104` — `ReadyStateManifest::id()` (existing JCS + BLAKE3 structural id).
- [../accepted/ADR-002-signature-format-jcs.md](../accepted/ADR-002-signature-format-jcs.md) — JCS (RFC 8785) canonicalization.
- [A1_SOURCE_TREE_PROFILE.md](A1_SOURCE_TREE_PROFILE.md) — `source_tree_hash` (`materialized_source_tree_hash`).
