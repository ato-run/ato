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

Every builder receipt is transported as the exact RFC 8785 JCS payload bytes,
base64-encoded, plus the builder's Ed25519 authentication. The API verifies
the signature over those decoded bytes and parses them; it must not reconstruct
the signed payload with a second canonicalizer. Shared vectors under
`crates/snapshot/tests/fixtures/authoring_evidence_v1/` pin the wire bytes.

Clean Replay, restore verification, and Ready-State Seal receipts each bind:

- receipt, Authoring Session, Capsule revision, source closure, and normalized
  Program Intent identities;
- the previous receipt digest in the evidence chain;
- issued-at and expiry instants for freshness enforcement;
- the builder key id and signature.

The restore receipt directly follows the Clean Replay receipt. The Seal receipt
directly follows the restore receipt and separately retains the Clean Replay
reference, so omission or substitution of either hop fails closed.

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

### Published media repair completion

A screenshot repair for an already-published Seal reuses that exact Seal and
does not create a new execution or installation identity. The builder restores
the Seal, performs readiness verification, captures and quality-checks a PNG,
and signs a `MediaRepairReceiptV1` that binds the PNG digest and quality report
to the existing Seal receipt.

Completion is an idempotent evidence handoff. The builder serializes the signed
receipt and PNG once, then resends those exact bytes while both the claim lease
and receipt remain fresh. Transport failures, HTTP 5xx responses, and malformed
success acknowledgements are retryable. HTTP 409 is a terminal domain refusal.
If retryable completion cannot be acknowledged before the earlier deadline, the
builder must not send the job-failed callback: the API retains the claimed
state and immutable completion evidence remains safe for an exact resend. Logs
may contain only a bounded error code and trace id, never response internals,
lease tokens, receipt bytes, or screenshot bytes.

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
