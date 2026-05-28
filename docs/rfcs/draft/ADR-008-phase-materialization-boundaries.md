---
title: "ADR-008: Phase materialization separates reusable build outputs from local execution"
status: draft
date: "2026-05-22"
author: "@egamikohsuke"
related:
  - "BUILD_MATERIALIZATION.md"
  - "DEPENDENCY_DERIVATION_CACHE.md"
  - "../accepted/CAPSULE_DEPENDENCY_CONTRACTS.md"
---

# ADR-008: Phase materialization separates reusable build outputs from local execution

## Context

AFFiNE is useful for validating OCI orchestration, but it is too large to be
the first source-native acceptance target for remote materialization. Ato needs
an incremental path where install/build work can become reusable layers without
moving secrets, persistent state, consent, and session lifecycle off the local
machine.

The existing run-side build materialization already computes a build input
digest and records whether declared build outputs are still present in the
workspace. That record cannot restore deleted outputs from a local or remote
cache yet.

## Decision

1. `execution_id` and phase materialization keys are separate identities.
   `execution_id` represents a local launch world. A phase materialization key
   identifies reusable install/build outputs and must not depend on secret
   values, persistent state, session IDs, or allocated ports.
2. `run` remains local-only. Phase materialization may precompute install/build
   outputs, but Ato still performs consent, secret injection, state mounting,
   network policy, and session supervision locally.
3. Output contracts are split. Build outputs such as `dist/` are recipe/build
   declarations. Dependency outputs such as `node_modules`, Yarn PnP metadata,
   pnpm projections, and uv environments stay behind ecosystem adapters until
   their relocatability contract is explicit.
4. The first MVP captures only declared build outputs into the immutable local
   blob store and projects them back into the workspace when a matching build
   materialization record exists but its outputs are missing.

### MVP Contracts

#### Layer projection

- The CAS payload is immutable and is never executed or mutated in place.
- The local MVP projects declared build-output paths back under the same build
  working directory before `run` starts.
- Projection uses the existing copy/clone projection ladder. The workspace view
  may be writable for runtime compatibility, but it must not share mutable inodes
  with the CAS payload. Later session filesystem views may tighten this into a
  read-only mount or overlay without changing the layer hash contract.
- Projection is non-destructive. If a declared build-output target already
  exists in the workspace, projection fails instead of replacing it. With
  `--no-build`, this is a hard failure; in a normal run, Ato falls back to the
  local build command, which owns its usual derived-output behavior.
- Projection is staged under the workspace-local temp directory and then
  committed by renaming each declared output into place. If a later output
  cannot be committed, already committed outputs from that projection attempt
  are rolled back.
- Projection and local build fallback for the same build output
  materialization key run under the same derivation lock. This serializes
  concurrent `ato run` processes that would otherwise touch the same output
  targets.

#### Relocatability

- The first MVP accepts only build outputs already declared as build outputs and
  treats those paths as relocatable within the same build working directory.
- Declared build-output entries must be regular files or directories. Symlinks
  are rejected at the root and inside output trees. Unsupported file types
  (device files, FIFOs, sockets, etc.) are rejected. On Unix, files with multiple
  hard links are rejected for this MVP instead of trying to prove whether the
  link target is safely inside the output contract.
- The layer key includes the platform profile. Cross-platform substitution is a
  cache miss even when source and output contract digests match.
- The layer key includes the phase, materializer schema version, and projection
  algorithm version. Target label, build command, working directory, and
  runtime/toolchain identity are carried by the build input digest.
- Dependency trees are excluded from this layer contract. Their projection shape
  and relocation policy belong to ecosystem adapters before they can become
  phase outputs.

#### Verification

- A persisted build materialization record stores the phase input digest, output
  contract digest, platform profile, materialization key, and blob hash.
- A warm projection recomputes the output contract and materialization key before
  reading the blob.
- The client verifies both the blob manifest hash and the payload tree hash
  before projection. A verification failure cannot satisfy `--no-build`; a
  normal run may fall back to executing the local build.

#### Remote CAS lookup, read-only MVP

- Remote lookup is optional and disabled by default. The first MVP enables only
  a file-backed mirror through `ATO_PHASE_MATERIALIZATION_REMOTE_ROOT`.
- Remote build submit is out of scope. A miss never asks a server or worker to
  build; normal runs fall back to the local build executor, while `--no-build`
  keeps its hard-fail semantics.
- Remote blobs are never projected directly into the workspace. A remote hit is
  validated, imported into the local immutable CAS with staging plus atomic
  rename, verified again locally, and only then projected with the same local
  projection path.
- Secrets, persistent state, session IDs, allocated ports, local environment
  values, and user workspace state are not sent to the mirror. The lookup path
  uses only the materialization key and layer metadata for the declared build
  output contract.
- This MVP does not define production HTTP CAS, signing policy, trust registry,
  dependency layer materialization, or package-manager relocation.

#### Remote CAS export, file-backed MVP

- File-backed export writes the same remote layer layout consumed by read-only
  remote lookup, so a later producer can reuse the artifact writer without
  changing the mirror contract.
- Export stages and verifies the blob before writing `layer.json`, then publishes
  the completed directory with rename. Readers do not observe a complete
  `layer.json` pointing at an incomplete payload from this writer.
- Export is idempotent when the existing remote layer is valid and identical to
  the requested local layer. An invalid or different existing remote layer is
  reported and not overwritten.
- Export is not trust or signing policy. It only proves the file-backed writer
  uses the same metadata and hash verification boundaries as lookup.
- Export is not remote build submit. It serializes an already captured local
  build-output layer and does not widen scope to dependency layers or production
  remote CAS transport.

#### Artifact producer contract boundary

- ADR-008 defines phase build-output local/remote layer mechanics:
  materialization records, immutable blobs, file-backed CAS exchange, and local
  projection before run.
- ADR-009 defines the artifact build producer request/response boundary for
  future remote-first build and output-first install flows.
- `artifact_build_id` is the ADR-level reusable build artifact identity.
  Current phase materialization keeps `materialization_key` as its existing
  implementation key and v0 compatibility alias.

## Alternatives Considered

### Option A: Start with deps and build layers together

- 利点: remote materialization would cover more setup work immediately.
- 欠点: dependency trees vary by package manager, native addon behavior, postinstall side effects, symlink topology, and platform-specific optional packages.

### Option B: Keep build output reuse as workspace-local state only

- 利点: no blob-store or projection changes.
- 欠点: the path cannot become a signed substitute flow and cannot restore an output layer after the workspace copy is removed.

### Option C: Use `execution_id` as the remote/build cache key

- 利点: one identity to explain.
- 欠点: local secrets, state, ports, and session details fragment reusable install/build outputs and create privacy pressure at the server boundary.

## Consequences

- Good: `dist/`-style relocatable outputs can exercise `key -> local CAS -> projection -> run` before dependency adapters are generalized.
- Good: future remote CAS lookup can reuse the same phase boundary without changing the local execution trust boundary.
- Bad: the first MVP still runs install/provision locally and does not speed up package-manager dependency resolution.
- Bad: build outputs that depend on runtime absolute paths or writable generated state need later adapter/policy work before they can be shared safely.

## Follow-up

- Add npm-only dependency output constraints and same-platform projection before widening to pnpm, Yarn, uv, Cargo, or mixed monorepos.
- Add remote CAS lookup only after local build-output projection is covered by fixture and regression tests.
