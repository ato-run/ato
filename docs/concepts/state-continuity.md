# Instance, Run, Lease and state continuity

An **Instance** is the mutable application ownership/data context operated by
the portable application's local or hosted lifecycle. An immutable Capsule can
be imported into independent Instances. A **Run** is one active evaluation;
stopping a Run need not delete its Instance. In the computation-root lifecycle,
Runs instead advance local computation branches. These profiles share the
immutable-Capsule/mutable-Run distinction without making their storage layouts
interchangeable.

A **Lease** is a time-bounded, fenced authorization/assignment to execute work.
It belongs to orchestration, not Capsule identity. An Instance identifier,
current PID, host endpoint and lease token are not interchangeable with K or D.

**State** is a purpose-specific projection of a Computation. Runtime memory,
declared filesystem state slots, saved JSON data, assets and external database
state have different capture and restoration boundaries. Credentials and
external connections are rebound through Bindings; secret values do not become
persisted identity.

## Snapshot and evidence

A **Ready-State Snapshot** is a physical artifact captured at a selected ready
point under host/backend compatibility conditions. Readiness is a Contract
property. The snapshot is a Materialization, not the Capsule's universal
identity. The [snapshot overview](../snapshot.md) distinguishes workspace,
VM and portable Instance snapshots.

**State Continuity Evidence** is the evidence needed to justify a particular
continuation claim: a known starting state, controlled capture/quiescence,
verified snapshot/closure, writer fencing, restore/migration outcome, and
post-restore observations linked to the relevant attempts. This is an
explanatory evidence requirement, not a newly adopted universal wire schema.
No generic `StateContinuityEvidence` type is asserted by this documentation.

K equality and a successful fresh-instance scenario do not prove preservation
of existing data across a restart or upgrade. Continuity across migration also
needs evidence about state format compatibility, external effects, concurrent
writers, ownership and recovery. Replaying an effect may require explicit
authorization and deduplication; an equal ContractRef does not make it safe.

The v4 [Instance snapshot draft](../rfcs/draft/PORTABLE_INSTANCE_SNAPSHOT.md)
defines bounded saved-data closure and requirements for that profile. Do not
generalize it into arbitrary database or process-memory migration.
Exporting changed saved data can seal a new snapshot-bound Contract; it does
not mutate the frozen K or bytes of the original bundle.
