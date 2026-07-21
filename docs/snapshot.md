# Snapshot

## Definition

A Snapshot is an immutable cache of the started state of exactly one
[Execution Identity](execution-identity.md). It exists to restore that resolved
launch contract quickly on a compatible runner.

```text
capsule.toml -> ato.lock.json.execution_contract -> Execution Identity
                                                     |-- cold reconstruction
                                                     `-- Snapshot cache
                                                            + External State
                                                            -> Session
```

Runtime filesystems, dependency outputs, OCI images, and application build
outputs are immutable inputs to the execution contract. They are not called
Snapshots by themselves.

## Wire formats and migration

New artifacts use `ato.snapshot-manifest/v1` and require:

- `snapshot_id`, derived from the canonical manifest payload without hashing
  the `snapshot_id` field itself
- exactly one `execution_id`
- backend/VMM/kernel/CPU-template/codec/runner compatibility requirements
- memory, VM state, and disk-layer references
- restore contract and capture policy
- capture provenance
- sanitization and redacted secret-scan attestations

Legacy `ato.ready-state/v1` remains readable for inspection. It is never
eligible for Capsule v1 lookup directly. Explicit migration verifies or
supplies the resolved v1 Execution Identity and writes a new immutable
`ato.snapshot-manifest/v1` with a new `snapshot_id`; legacy bytes are not
reinterpreted in place.

## Selection and quarantine

Selection first requires exact `execution_id`, then proves compatibility.
Capsule name, target label, source commit, runner name, or recency cannot replace
exact identity matching. Ranking is applied only after both gates pass.

If an accepted artifact later fails validation, the catalog record is marked
quarantined. Manifest bytes and `snapshot_id` remain unchanged, and quarantined
records are ineligible for selection.

## Capture acceptance

`seal_at.command` is an acceptance condition, not the exact instant at which a
candidate is first captured:

```text
capture immutable candidate
  -> restore into disposable Session
  -> run seal_at.command as exact argv
  -> exit 0: accept the original candidate
  -> otherwise: reject and optionally recapture within bounds
  -> always destroy workload, leases, state attachments, overlay, and Session
```

Validation uses no production secrets, user state, or Ato user identity.
Secret scanning is an attestation, not a proof that no secret can exist.
Timeout terminates the validation process tree. Attempts and the total deadline
are bounded and receipted.

## Capture policies

`running` is used only when the live workload needs no External State. A
Capsule requiring External State fails this eligibility gate closed.

`workload_idle` uses the following fixed sequence:

```text
synthetic preparation
  -> StopWorkload
  -> revoke and attest every placeholder
  -> capture candidate
  -> disposable restore
  -> attach synthetic validation bindings
  -> start workload
  -> run seal_at.command
  -> stop and destroy disposable resources
```

A real run restores the workload-idle Snapshot, attaches schema-compatible real
External State, delivers binding leases, starts the workload, verifies
readiness, and only then exposes the Session. Partial failure, timeout, and
cancellation clean up the workload, leases, volumes, overlay, and Session.

## External State boundary

External State uses a separate volume or injection boundary and is always
declared with `snapshot = "exclude"`. Its name, target, access, schema, and
exclusion contract are identity-bearing. Its concrete owner, opaque reference,
generation, bytes, secret values, and identity assertions are not.

Before read-write attach, schema incompatibility fails closed. Shared Snapshot
layer references are checked not to include External State layers. Session
Receipts carry only opaque references, generations, and compatibility evidence.

## Backends

`crates/snapshot` keeps backend details behind `SnapshotBackend` and the v1
compatibility contract. Firecracker is the real x86_64/KVM backend;
`FakeSnapshotBackend` exercises build/restore without KVM; QEMU and Kata remain
reserved implementations. Backend and runner constraints affect Snapshot
compatibility, not Execution Identity.

References:

- [Capsule v1 Execution Identity and Snapshot Model](rfcs/accepted/CAPSULE_V1_EXECUTION_MODEL_SPEC.md)
- [Capsule v0.3 to v1 migration](capsule-v1-migration.md)
- [Snapshot compatibility](snapshot-v1-compatibility.md)
- [Snapshot run control API](api/snapshot-run-control.md)
