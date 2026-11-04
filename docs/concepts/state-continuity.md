# Instance, Run, Lease and state continuity

Status: Current technical reference.

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

## Data and identity: one JSON object

Take an application that stores one small document:

```json
{"items":[{"id":1,"name":"first","quantity":2},{"id":2,"name":"second","quantity":1}]}
```

The document can answer four different questions. Keeping them apart is what
makes a continuity claim reviewable:

| Question | In this example |
|---|---|
| What counts as the same data (K)? | A declared observation over the document. An exact-content observation binds the document bytes by digest, so a re-indented copy or one whose keys are written in a different order is a different byte sequence with a different digest, even though a JSON reader sees the same items. A semantic observation such as "the item list contains these entries in this order" can accept that copy. Exact bytes and semantic equality are different Contracts; choose one and state it. |
| How is the data realized (D)? | The bytes under an Instance's declared state or data directory, an embedded JSON Data Resource object in the bundle, or browser storage. These are realization choices; a different D can satisfy the same K. |
| What is the Port? | In the model frame of this page, the declared data resource slot and the read/replace operations the application exposes over it. The slot is logical and names no path. The v4 saved-data wire names the same boundary with `slot` + `protocol` (for example `main` and `ato.data.json@1`) rather than with a core `PortId`. |
| What is the Binding? | The Run-specific mapping from that slot to a physical store: path or mount, permissions, and the external resource it resolves to. Binding is realization state; secret values stay runtime inputs and do not enter K or D. |

The v4 saved-data profile adds the snapshot identity to K instead of rewriting
it: `K_saved = K_base + snapshot identity`, with the declared Derivations
unchanged ([Instance snapshot draft](../rfcs/draft/PORTABLE_INSTANCE_SNAPSHOT.md)).
The snapshot carries content references for declared resources and Assets.
Local Instance IDs, Asset IDs, storage paths, grants and signed URLs stay
outside K and D.

Implemented in the current v4 saved-data profile (source and the snapshot
draft's implementation boundary):

- the v4 wire object (`ato.portable-instance-snapshot/1`) that names declared
  resource slots and Assets, and its snapshot digest bound into K_saved;
- durable local Instance materialization with independent Asset IDs, and
  sealing the declared saved data when a Run stops;
- Hosted restore of declared browser state, JSON Data Resources and Assets
  before launch;
- a checkpoint of a declared filesystem-state slot (`ato.state.filesystem@1`)
  in the local Instance path.

Not implemented, or not yet proven:

- browser-state flush and capture from a live Hosted Instance, and the PWA
  export UI;
- field-aware Asset alias resolution inside saved JSON;
- injection of a filesystem checkpoint into Process/OCI routes;
- arbitrary filesystem capture, process-memory capture or external database
  migration;
- a proven Hosted round-trip. The draft records no staging acceptance that
  proves it; later progress and staging records describe saved-data
  round-trips, but they were not re-executed for this page and are not
  generalized here.

Data identity is not an operation history. A Record or Trace is evidence of
observed Evolution ([Capsule and Run](capsule.md#record)); replaying that
history is one possible Materialization, not what makes the data the same.
Two equal documents can arrive through different histories, and preserving a
history does not by itself preserve the documents. A continuity claim should
say which of these it has: equal bytes, a satisfied semantic observation, a
restored realization, or a reproduced history.

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
