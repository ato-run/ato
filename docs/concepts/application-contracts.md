# Capsule, Contract K, Derivation D and Source

Status: Current technical reference.

Ato preserves a point from which work can continue. The
[Computation model](computation.md) explains that point independently of the
process, filesystem, container or VM used to realize it. The portable
application profile adds a bounded, observable Contract for an application.
It does not turn a deployment tool into a new semantic primitive.

## Profile map

Both profiles use the same model — Computation, Capsule and Run, Contract K,
Derivation D, Port and Binding. The map records which identity and transport
each profile uses and which CLI commands operate on it. It describes the
current tree and the workspace design direction; it does not amend the
accepted RFCs or turn one root into a view of the other.

| Profile | Logical root and transport | CLI surface |
|---|---|---|
| Common model (shared vocabulary) | Computation, Capsule/Run, K, D, Port and Binding. Shared vocabulary, not a wire profile: it defines no root of its own. | none |
| Computation-root | `ComputationRef` derived from canonical ComputationObject bytes; wire v2, `index.root` | `init`, `resume`, `stop`, `encap`; `run` on a v2 file |
| Portable application | `ContractRef` derived from canonical BoundContract bytes; wire v3 `ato.portable-application/1` and wire v4 `ato.portable-application/2` | `pack`, `form`, `reform`, `app`, `export`, `export-plan`; `run` on a v3/v4 file |

`run` is a reader, not a converter: the
[bundle reader](../../lib/objects/src/portable_bundle.rs) dispatches on
`index.version` and sends v2 to the computation-root path and v3/v4 to the
portable application path. The same reader refuses an unknown version instead
of falling back between profiles.

The command notes below are read from the current CLI dispatch
([apps/cli/src/lib.rs](../../apps/cli/src/lib.rs)):

- `encap` materializes a computation-root point as a v2 `.capsule`.
- `pack` compiles `ato.capsule/2` authoring input into a v3 `.capsule`;
  `export` repacks a portable application as v4 with its dependency transport.
- `form` is the local Formation front end for `ato.capsule/1` source
  candidates. It forms and verifies against the contract-root identity model
  and reports a `ContractRef`, but it publishes content-addressed artifacts
  rather than a v3/v4 `.capsule` file; `pack` is the bundle-producing front
  end.
- `reform` builds a candidate under the frozen K and refuses a v2 input. The
  candidate writer emits a v3 portable-application bundle and carries the
  frozen Contract bytes unchanged; it does not preserve a v4 snapshot shape.
- `app import` and `app start` accept v3/v4 and refuse a v2 computation-root
  bundle; `app export` writes the bundle currently selected by the Instance.
- `export-plan` previews a portable export from a v3/v4 bundle. `upload`
  publishes a computation-root object graph, not a `.capsule` bundle.

The accepted [Computation Architecture](../rfcs/accepted/COMPUTATION_ARCHITECTURE.md)
and [Bundle](../rfcs/accepted/CAPSULE_BUNDLE.md) RFCs describe the first profile.
The portable application RFCs are still
[implementation-track drafts](../specifications/README.md#portable-application-implementation-track);
their status is unchanged by this map.

The SHA-256 of a `.capsule` file identifies exact transport bytes. It is not a
substitute for either logical root. Never reinterpret a ContractRef as a
ComputationRef. The wire table and validation rules are in
[Portable Capsule bundle](../capsule.md).

## Open decisions

Four decisions about this boundary are recorded in the
[mismatch register](../rfcs/draft/DOCUMENTATION_AUTHORITY_MISMATCHES.md) and
remain unresolved. This page does not choose an answer.

| Decision | What remains open | Status |
|---|---|---|
| [M1](../rfcs/draft/DOCUMENTATION_AUTHORITY_MISMATCHES.md#m1--capsule-identity-across-profiles) | Scope the accepted architecture and bundle text to the computation-root profile, or define a different normative relation for the contract-root profile | Unresolved |
| [M2](../rfcs/draft/DOCUMENTATION_AUTHORITY_MISMATCHES.md#m2--public-packaging--formation-commands) | Say which accepted lifecycle is profile-specific; adopt the implemented public application command contract | Unresolved |
| [M3](../rfcs/draft/DOCUMENTATION_AUTHORITY_MISMATCHES.md#m3--initial-v3-scope-versus-later-implementation) | Consolidate the versioned profile specifications and their extensions in an explicit adoption record | Unresolved |
| [M4](../rfcs/draft/DOCUMENTATION_AUTHORITY_MISMATCHES.md#m4--draft-status-and-receipt-authority) | Adopt one Contract/Derivation/Receipt specification, including required execution provenance per acceptance path | Unresolved |

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
[compatibility](compatibility.md), the
[research tour](../guides/research-tour.md) for a shorter path through the
model, and the canonical [glossary](../glossary-reference.md).
