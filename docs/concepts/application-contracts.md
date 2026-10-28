# Capsule, Contract K, Derivation D and Source

Status: Current technical reference.

Ato preserves a point from which work can continue. The
[Computation model](computation.md) explains that point independently of the
process, filesystem, container or VM used to realize it. The portable
application profile adds a bounded, observable Contract for an application.
It does not turn a deployment tool into a new semantic primitive.

## Two identity profiles

| Profile | Logical identity | Transport |
|---|---|---|
| Computation-root | `ComputationRef`, derived from canonical ComputationObject bytes | wire v2, `index.root` |
| Portable application | `ContractRef`, derived from canonical BoundContract bytes | wire v3 `ato.portable-application/1`; wire v4 `ato.portable-application/2` |

The accepted [Computation Architecture](../rfcs/accepted/COMPUTATION_ARCHITECTURE.md)
and [Bundle](../rfcs/accepted/CAPSULE_BUNDLE.md) RFCs describe the first profile.
The portable application RFCs are still
[implementation-track drafts](../specifications/README.md#portable-application-implementation-track).
The profile distinction describes current code and the workspace design
direction; it does not silently amend the accepted RFCs.

The SHA-256 of a `.capsule` file identifies exact transport bytes. It is not a
substitute for either logical root. Never reinterpret a ContractRef as a
ComputationRef.

## K, D and Source

**Contract K** specifies the observations a realization must satisfy. In the
application profile, the canonical `BoundContract` contains its schema and
bound requirements. Captured expectations become values before sealing. The
verifier compares a Run against that frozen K; it must not replace the
expectations with whatever the Run happened to return.

**Derivation D** describes one explicit way to realize K: immutable input refs,
steps, runtime requirements, logical Ports and other declared realization
conditions. Its `DerivationRef` is distinct from K. A bundle can declare several
Ds for one K; the caller selects a declared D when there is more than one.
Admission checks the selected route's host capabilities separately from bundle
integrity. An unavailable runtime does not make the bundle corrupt.

**Source** is input to formation or to a Derivation. The immutable workspace
closure binds actual source bytes; a Git URL, mutable branch, manifest or upload
location alone is not that closure. Source identity enters K only when K
requires it, for example through `ato.contract.workspace@1`. Different sources
can satisfy one behavioral K when that K does not bind source identity.

Python processes, OCI containers and static serving are Adapter/realization
choices. Their mechanics do not introduce new top-level Capsule kinds.

See [Formation and verification](formation-verification.md),
[compatibility](compatibility.md), and the canonical
[glossary](../glossary-reference.md).
