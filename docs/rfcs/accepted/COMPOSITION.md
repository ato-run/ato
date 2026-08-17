# Composition

Status: Accepted

Composition combines immutable child `ComputationRef` values using explicit
Connections and exports. Pure wiring belongs to `ato-computation`; operational
semantics belongs to `ato-compose`.

Validation enforces canonical connection order, linear endpoint use, matching
Protocols, compatible roles, exact parent-boundary exports, resource budgets,
and transitive child validity. Reduction derives child transitions without
publishing them, synchronizes complementary equal payloads, seals the parent
successor, and publishes one parent transition.

Composition does not create Placement, Cluster, ServiceGraph, WebApp, or other
workload-specific semantic types.

Every authored Port names its owner Node explicitly. Runtime projection derives
per-node physical endpoint environment from Connections; it never infers owners
from Port name prefixes or silently assigns an unmatched Port to a first process.
An internal client Port without a Connection is invalid.
