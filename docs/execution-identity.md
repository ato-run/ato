# Execution Identity

## Overview

Execution Identity is Ato's sole exact identity for an execution. It identifies
the platform-resolved launch contract: source, target, runtime, dependency and
build outputs, launch command, normalized environment, filesystem view,
policies, guest surface, and External State schema contracts.

It is not a source hash, a Snapshot ID, a runner ID, or a Session ID.

The normative definition is the
[Capsule v1 Execution Identity and Snapshot Model](rfcs/accepted/CAPSULE_V1_EXECUTION_MODEL_SPEC.md).
Migration behavior is documented in
[Capsule v0.3 to v1 migration](capsule-v1-migration.md).

## Canonical form

`ato.lock.json` persists the resolved contract separately from metadata:

```text
ato.lock.json
  execution_contract   identity-bearing resolved launch contract
  execution_id         stored canonical digest
  provenance           non-identity metadata
  diagnostics          non-identity metadata
  evidence             non-identity metadata
  generated_at         non-identity metadata
```

The ID is computed only from `execution_contract`:

```text
execution_id =
  BLAKE3(
    "ato.execution-contract/v1"
    || NUL
    || JCS(lock.execution_contract)
  )
```

A durable lock is rehashed when read. Missing pairs, malformed contracts, and
digest mismatches fail closed.

## Identity-bearing conditions

- immutable source reference and content digest
- target OS, architecture, ABI/libc, and app-observable CPU/GPU features
- resolved runtime reference and artifact digest
- dependency derivation and actual output digests
- application build-output digests
- exact launch argv and cwd
- normalized environment names and value digests; secret binding names only
- filesystem view and layer identities
- network, capability, and filesystem policies
- guest protocol/surface requirements
- External State name, target, access mode, schema, and Snapshot exclusion rule

Every identity-bearing artifact digest is finalized before `execution_id` is
issued. Linux x86_64 and Linux arm64 therefore have different IDs.

## Conditions outside Execution Identity

- Snapshot ID, format, backend, VMM restore constraints, or capture time
- runner/provider/machine names
- Session ID, dynamic ports, assigned IPs, and timestamps
- diagnostics, logs, evidence URLs, and builder names
- External State owner, instance reference, generation, or data
- secret values and identity assertions

These values may be recorded in a redacted Session Receipt when needed, but do
not create another exact execution identity.

## Relationship to Snapshot

A Snapshot is an immutable cache subordinate to exactly one Execution Identity.
The selection gate is:

```text
exact execution_id
  -> proven Snapshot compatibility
  -> ranking among eligible candidates
```

Snapshot format changes may change `snapshot_id`, but never `execution_id`.
When no compatible Snapshot exists, policy may permit cold reconstruction of
the same resolved contract. Every reconstructed identity-bearing condition is
verified before External State is attached or a Session starts.

## Receipts and internal graph data

ExecutionGraph, ExecutionPlan, Realization, Materialization, and the historical
declared/resolved/observed graph facets remain useful internal planning and
diagnostic structures. They are not competing public exact execution
identities. Public APIs use `execution_id`; internal facet fields are legacy or
diagnostic evidence and must not be used for Snapshot lookup.

Session Receipts record the selected Snapshot, runner, dynamic endpoints,
attached opaque state references and generations, launch mode, and validation
evidence without recording state contents, secret values, or identity
assertions.

## Same source is not the same execution

```text
same source + runtime digest A  -> execution X
same source + runtime digest B  -> execution Y

same source + linux/x86_64      -> execution P
same source + linux/aarch64     -> execution Q

same source + network denied    -> execution M
same source + network allowed   -> execution N
```

Conversely, selecting a different compatible Snapshot or runner for the same
resolved launch contract does not change Execution Identity.
