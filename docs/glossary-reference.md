# Glossary

Status: Current technical reference.

Canonical terminology for current explanatory documentation. Accepted RFCs own
normative contracts; the [specification index](specifications/README.md) labels
implementation-track drafts. Definitions below do not amend those RFCs.

| Term | Meaning |
|---|---|
| Computation | Evolving residual computation: what can still happen from the current point. |
| Evolution | Transition `C ──α──▶ C′`; build/setup/install/launch can be Evolutions. |
| Composition | Explicit compatible Port wiring whose result is another Computation. |
| Capsule | Immutable, sealed, addressable continuation. Computation-root identity is `ComputationRef`; portable application identity is `ContractRef`. |
| Contract K | Obligations/observations a realization must satisfy. The application profile binds requirements into canonical `BoundContract` bytes. |
| Derivation D | One explicit realization route bound to immutable inputs and K, addressed by `DerivationRef`; selected independently of K identity. |
| Source | Formation/Derivation input. An immutable source closure is distinct from a mutable repository URL or branch. It enters K when K binds source identity. |
| Formation | Producing a candidate realization and bound evidence from source/authoring input. Seal may defer named runtime checks. |
| Verification | Comparing actual observations with unchanged K. Accepted Run verification requires all required observations satisfied. |
| Receipt | Evidence of verification, including K, D, target, observations, overall satisfaction and path-specific execution/transport evidence. |
| Instance | Mutable application ownership/data context with potentially successive Runs; not immutable Capsule identity. |
| Run | Mutable active evaluation resumed from a Capsule; its scope and storage are profile-specific. |
| Lease | Fenced, time-bounded orchestration assignment/authorization; not K, D or state identity. |
| State | Purpose-specific projection `StateK(C)`, such as filesystem, UI, protocol or saved application data. |
| Ready-State Snapshot | Physical artifact captured at a selected ready point with realization compatibility conditions. |
| State Continuity Evidence | Evidence supporting a bounded capture/restore/migration claim. An explanatory category, not a new universal wire schema. |
| Continuous Patching | Candidate generation, rebuild, re-verification and controlled rollout with independent state/effect/security checks; full automation is not implied. |
| Derivation Transformation | Constructing a changed D under frozen canonical K and verifying it without recapturing K. |
| Record | Durable evidence of observed Evolution; not the current Computation or its identity. |
| Trace | Ordered or causally related view of past Records. |
| Materialization | Physical encoding/reconstruction strategy; availability constrains realization, not logical identity. |
| Binding | Mapping a logical requirement to a Run-specific physical resource or external dependency. Secret values remain runtime inputs. |
| Port | Typed interaction boundary, currently `PortId`/`PortDef` on a Computation Boundary. |
| Protocol | Owns interaction semantics; Kernel remains payload-opaque. |
| Adapter | Connects physical I/O to the logical Protocol. |
| Endpoint | In core composition, `{ node, port }` selects a child Port. A physical runtime endpoint is a separate realization concern. |
| PortRef | Persistent logical Port reference in the model/future direction; not asserted as an implemented core type. |
| ComputationObject | Canonical `{ semantics, boundary, residual }` value. |
| ComputationRef | BLAKE3 address of canonical ComputationObject bytes; computation-root Capsule handle. |
| ContractRef | SHA-256 address of canonical application-profile BoundContract bytes; not a ComputationRef. |
| Execution Identity | Profile-specific logical refs and actual attempt/realization evidence. No universal aggregate type is assumed. |
| `.capsule` | Portable transport. v2 roots at a ComputationRef; v3/v4 at a ContractRef. File SHA-256 proves exact transport bytes. |
| Replay | Materialization reconstructing a target through recorded interactions from a verified anchor; not universal deterministic replay. |
| Resume | Realize and continue a Capsule as a Run. |
| Fork | New Run/branch future without mutating the immutable origin. |
| Lineage | Evidence relating a branch/fork to origin points and Records, separate from identity. |

See [Port and Endpoint](concepts/computation.md#port-and-endpoint-theory-vs-implementation),
[K and D](concepts/application-contracts.md), [state continuity](concepts/state-continuity.md)
and [compatibility](concepts/compatibility.md) for qualifications.

<!-- Compatibility anchors from the previous documentation entry. -->
<a id="legacy-terminology"></a>
