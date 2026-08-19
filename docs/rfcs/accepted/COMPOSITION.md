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

## Portable Capsule composition projection

The Capsule Network may ask a Connected Runner to combine two through eight
already-verified portable Capsules. The Runner imports each immutable root into
one object store and authors a normal `capsule.compose@1` Computation through
`ato-compose`; it does not introduce a Network-specific composition semantic.
Inputs receive deterministic node names (`capsule01`, `capsule02`, …), and each
input boundary is re-exported under a deterministic node-qualified name. The
operation writes a new portable bundle and never mutates an input bundle.

Access authorization, visibility ceilings, and the intersection of eligible
principals are control-plane policy. They are not encoded into the Computation
or delegated to the Runner. Transport URLs and Runner credentials remain
runtime-only and are not exported into the result bundle.
