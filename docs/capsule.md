# Portable Capsule bundle

A `.capsule` file transports an immutable root and its validated object closure.
The file's SHA-256 establishes byte equality; logical identity depends on its
declared profile.

| Wire | Profile/root | Authority |
|---|---|---|
| v2 | computation-root, `index.root: ComputationRef` | [accepted bundle RFC](rfcs/accepted/CAPSULE_BUNDLE.md) |
| v3 | `ato.portable-application/1`, `root_contract_ref: ContractRef` | [draft](rfcs/draft/PORTABLE_APPLICATION_BUNDLE_V3.md), [reader](../lib/objects/src/portable_bundle.rs) |
| v4 | `ato.portable-application/2`, ContractRef with bounded Instance snapshot support | [snapshot draft](rfcs/draft/PORTABLE_INSTANCE_SNAPSHOT.md), same reader |

Validation checks schema, canonical digests, closure and declared route
consistency before execution. Do not cast refs between profiles, use unknown
profiles as a fallback, or substitute transport bytes for semantic identity.
Host runtime/platform availability is a separate admission question for the
explicitly selected D. Real HTTP observations may use dynamic endpoints on a
logical Port; static-file existence checks belong to static routes.

Use [Getting Started](guides/getting-started.md) for portable application
`pack` / `run`, or [Local lifecycle](run.md) for computation `encap` / `run`.
Both paths preserve immutable roots; their formats and formation inputs differ.
[Specifications](specifications/README.md) separates accepted authority from
implementation-track contracts and [Concepts](concepts/application-contracts.md)
explains K, D and Source.
