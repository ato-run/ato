# Architecture

Read [Core architecture](../core-architecture.md) for the semantic dependency
direction, then use these implementation boundaries:

| Area | Responsibility / source |
|---|---|
| Computation, Kernel, composition | [accepted architecture](../rfcs/accepted/COMPUTATION_ARCHITECTURE.md), `lib/computation`, `lib/kernel`, `lib/compose` |
| CAS, Record, lineage, wire transport | `lib/objects`; [accepted bundles](../rfcs/accepted/OBJECT_BUNDLE.md) |
| Contract binding and verification | [Formation library](../../lib/formation/src/lib.rs) |
| admission and owned attempts | [Runtime attempt layer](../rfcs/draft/ADR-027-runtime-attempt-layer.md), `lib/runtime-attempt` |
| portable bundle execution | [shared portable application app](../../apps/portable-application/Cargo.toml), used by CLI and Formation worker |
| Hosted Formation frontend | [capsule/2 implementation](hosted-formation-capsule-v2.md); measurements are dated, not a rollout statement |
| Scenario verifier | [v1 implementation](scenario-verifier-v1.md); fresh-instance scope |
| physical I/O and realization | `extensions/adapters`, `extensions/materializers`; [snapshot overview](../snapshot.md) |
| human / agent interaction | [COOP](../coop.md); no extra Semantic Core primitive |

Placement, process supervision, sandboxing, provider credentials, networking,
leases and deployment policy stay outside the Semantic Core. Domain-specific
payload semantics belong to Protocols and Adapters. Logical Ports remain
separate from their Run-specific physical endpoints.

[Detected mismatches](../rfcs/draft/DOCUMENTATION_AUTHORITY_MISMATCHES.md)
separates adoption and stale-status questions from the established architecture.
