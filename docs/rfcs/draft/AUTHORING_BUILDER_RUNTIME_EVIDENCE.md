---
title: "Authoring Session Builder/Runtime Evidence"
status: draft
date: 2026-07-28
author: "@egamikohsuke"
ssot:
  - "crates/capsule/src/contract/authoring_intent.rs"
  - "crates/snapshot/src/authoring_evidence.rs"
---

# Authoring Session Builder/Runtime Evidence

## Decision

An Authoring Session is an evidence-producing workflow, not an execution
identity and not a Snapshot source.

```text
ProgramIntentDraft
  -> validate and normalize (capsule)
  -> resolver-generated capsule.lock
  -> isolated clean replay (snapshot-builder)
  -> classified replay diff
  -> quiesce and capture
  -> disposable restore verification
  -> ReadyStateSealReceipt
```

The draft and normalized-intent digests bind evidence, but neither may mint an
`execution_id`. The only exact execution identity remains
`ato.execution-contract/v1`, finalized from measured materialization output.

## Boundaries

### Capsule domain

`ProgramIntentDraftV1` is source-agnostic. Git, local-tree, archive, inferred
source, and `capsule.toml` adapters all call the same pure normalizer. Commands
are exact argv. Shell is representable only as an explicit escape hatch whose
interpreter argv remains visible.

The normalized contract preserves:

- ordered build steps;
- argv boundaries, cwd, requested environment names, and tool requirements;
- launch and readiness;
- declared build-output roots;
- binding requirements by name and kind, never values;
- unresolved review items, which make normalization fail closed.

The resolution lock is not accepted as author input. It is a separate,
resolver-produced clean-replay input.

### Builder domain

The builder owns ephemeral workspace creation and proves freshness. Clean
Replay accepts only:

- a resolved source closure;
- validated source overlays;
- a normalized Program Intent;
- a resolver-generated lock;
- explicitly allowed content-addressed caches.

It must not receive the authoring workspace, authoring processes, host
environment, host credentials, or unrecorded host tools.

`CleanReplayReceiptV1` is emitted by an authenticated builder and binds the
Authoring Session, all materialization inputs, execution contract, readiness,
effective isolation posture, and replay diff. The API verifies this receipt;
it never creates one.

### State classification

Every replay path is classified as exactly one of:

`source_overlay`, `build_output`, `seed_state`, `user_state`, `temporary`,
`sensitive`, or `unknown`.

Only declared build-output roots are auto-includable. Seed state requires an
explicit confirmation and receives a separate artifact identity. User state
and temporary paths are excluded. Sensitive and unknown paths block sealing.

### Ready-State Seal

Only a successful clean-replay runtime may be captured. The sequence is fixed:

```text
readiness
  -> classify diff
  -> verify no sensitive, identity, or user state
  -> quiesce
  -> capture
  -> disposable restore
  -> readiness
  -> post-restore screenshot
  -> ReadyStateSealReceipt
```

The Seal identity is distinct from a Capsule revision and from Execution
Identity. A Seal always references its clean-replay and restore-verification
receipts and remains reconstructible from source, normalized intent, and lock.

## Publish gate

Publication remains fail-closed unless all of these are independently present
and mutually bound:

1. validated normalized Program Intent;
2. resolver-generated lock;
3. authenticated successful Clean Replay receipt;
4. complete state classification with no included sensitive/unknown/user state;
5. restore-verified Ready-State Seal;
6. user-selected post-restore screenshot;
7. license and provenance acknowledgement.

Legacy submission is never an implicit fallback. A compatibility lane, if
retained, must be a separate route or explicit feature flag.

