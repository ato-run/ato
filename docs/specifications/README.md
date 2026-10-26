# Specifications

This index locates authority; it does not promote drafts into accepted specs.
When code and an accepted RFC disagree, record a **Detected mismatch** and seek
a maintainer decision. Do not infer the normative contract from merged code.

## Accepted specifications

These RFCs describe the computation-root architecture. Their unqualified
identity and CLI statements conflict with later portable-application code;
[M1 and M2](../rfcs/draft/DOCUMENTATION_AUTHORITY_MISMATCHES.md) track that
unresolved scope/adoption question. Do not apply those statements as a formal
specification of the application profile. That profile's implementation
contracts remain in the separate draft section below.

| Specification | Owns |
|---|---|
| [Computation Architecture](../rfcs/accepted/COMPUTATION_ARCHITECTURE.md) | semantic values, canonical identity and Evolution |
| [Composition](../rfcs/accepted/COMPOSITION.md) | closed composition and Port wiring |
| [Protocol Adapter](../rfcs/accepted/PROTOCOL_ADAPTER.md) | logical interaction, physical adapters and capture boundary |
| [Local Capsule Repository](../rfcs/accepted/LOCAL_CAPSULE_REPOSITORY.md) | branches, Runs, Records and lineage |
| [Materialization](../rfcs/accepted/MATERIALIZATION.md) | encoding, compatibility and restoration |
| [Object Closure Bundle](../rfcs/accepted/OBJECT_BUNDLE.md) | object closure and transport validation |
| [Capsule Bundle](../rfcs/accepted/CAPSULE_BUNDLE.md) | computation-root wire v2 |
| [Capsule CLI Lifecycle](../rfcs/accepted/CAPSULE_CLI_LIFECYCLE.md) | public computation lifecycle |

## Portable application implementation track

The following are **draft implementation contracts**, not formally adopted
specifications. Some describe implemented code; deployment and acceptance must
be established separately.

| Contract | Draft / implementation reference |
|---|---|
| contract-root wire v3 | [Portable Application Bundle](../rfcs/draft/PORTABLE_APPLICATION_BUNDLE_V3.md), [portable bundle reader](../../lib/objects/src/portable_bundle.rs) |
| authoring schema `ato.capsule/2` | [Authoring v2](../rfcs/draft/PORTABLE_APPLICATION_AUTHORING_V2.md), [Formation library](../../lib/formation/src/lib.rs) |
| multi-D and dynamic process/OCI routes | [Dynamic Derivations](../rfcs/draft/PORTABLE_DYNAMIC_DERIVATIONS.md) |
| wire v4 and saved state | [Instance Snapshot](../rfcs/draft/PORTABLE_INSTANCE_SNAPSHOT.md), [Local Instance](../rfcs/draft/PORTABLE_LOCAL_INSTANCE.md) |
| Contract K and Derivation D objects | [canonical authoring types](../../lib/formation/src/authoring.rs) |
| Verification Receipt | [verifier and receipt types](../../lib/formation/src/verify.rs), [ephemeral verification](../rfcs/draft/ADR-019-formation-ephemeral-verification.md), [coordinator receipt authority](../rfcs/draft/ADR-029-coordinator-receipt-authority.md) |
| Scenario verification | [implementation explanation](../architecture/scenario-verifier-v1.md) |
| process-boundary schemas | [IPC types](../../lib/ipc/src/lib.rs), [IPC golden fixtures](../../lib/ipc/tests/fixtures/) |

The initial v3 draft describes a static, single-D increment; code and later
drafts extend it. Do not mistake that initial scope for the full implemented
profile. [Detected mismatches](../rfcs/draft/DOCUMENTATION_AUTHORITY_MISMATCHES.md)
tracks the adoption/scope gaps without changing wire formats.

`docs/specs/`, `docs/contracts/` and `docs/connectors/` contain older reference
material retained for links and history. A filename containing “SPEC” or a JSON
schema in those directories does not by itself establish current authority.
Use the [inventory](../archive/2026/documentation-reorganization/inventory.tsv)
for their status and the [RFC index](../rfcs/README.md) for the full draft list.
