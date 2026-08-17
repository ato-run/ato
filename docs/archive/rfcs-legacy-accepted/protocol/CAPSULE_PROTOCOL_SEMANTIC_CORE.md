---
title: "Capsule Semantic Core"
status: accepted
date: 2026-08-13
author: "@egamikohsuke"
supersedes:
  - "../../archived/CAPSULE_V1_EXECUTION_MODEL_SPEC.md"
ssot:
  - "crates/capsule-core/"
related:
  - "CAPSULE_COMPUTATION_OBJECT_V1.md"
  - "CAPSULE_PROTOCOL_V1_COMPATIBILITY.md"
  - "../../draft/CAPSULE_COMPOSE_SEMANTICS.md"
---

# Capsule Semantic Core

## 1. Authority

The semantic primitive is `Computation`: a residual process governed by one
versioned semantics. A running semantic state need not be serialized or
content-addressed. A Capsule is a sealed, addressable Computation; it is not a
second Core primitive.

This specification supersedes the five-element semantic model in
[`CAPSULE_V1_EXECUTION_MODEL_SPEC.md`](../../archived/CAPSULE_V1_EXECUTION_MODEL_SPEC.md).

## 2. Sealing and identity

```text
runtime semantic state C
        | seal
        v
ComputationObject
        | canonical encode + BLAKE3
        v
ComputationRef
```

`ComputationRef` is a syntactically valid persistent reference in the sealed
Computation Object v1 reference domain. Like a Git object ID, the reference may
be unresolved or dangling; parsing it does not prove that bytes exist or match
the digest. `ResolvedComputation` is the separate value obtained after
canonical decoding and exact digest verification. A future
`SemanticallyValidComputation` boundary may additionally attest that the
selected Semantics accepts the residual and boundary.

The reference addresses the complete `ComputationObject`. Interpretation
authority is therefore inside the addressed object: changing its semantics,
boundary, or residual changes its identity.

The sealed object is exactly:

```text
ComputationObject {
  semantics: SemanticsId,
  boundary:  PortId -> PortDef,
  residual:  ContentRef
}

PortDef {
  protocol: ProtocolId,
  role:     RoleId
}
```

`SemanticsId` and `ProtocolId` are immutable, versioned identifiers. Optional
specification references and registry attestations are outside Core identity.
Canonical encoding and hashing are governed by
[`CAPSULE_COMPUTATION_OBJECT_V1.md`](CAPSULE_COMPUTATION_OBJECT_V1.md).

## 3. Boundary and names

A boundary is an interface signature: it declares stable `PortId` values and
their protocol and role. Protocol semantics determine which actions each role
may send or receive; Core does not reduce this to ingress, egress, or duplex.

Three identities must remain distinct:

```text
Name     runtime calculus channel; may be mobile
PortId   stable name within one sealed boundary
PortRef  persistent reference to (ComputationRef, PortId), defined above Core
```

The mapping from a boundary `PortId` to an internal runtime `Name`, child Port,
or other realization belongs to the semantics-specific residual. It is not a
Core field. Physical endpoints, bindings, grants, transports, file
descriptors, runtime adapters, and Connectors are also outside Core.

## 4. Evolution

Evolution is a semantics-defined relation:

```text
C --alpha--> C'
```

Neither side is required to have a `ComputationRef`. Sealing a state produces
an immutable representation when persistence, transfer, or persistent Port
identity is required. Observations, histories, records, traces, clocks, replay
evidence, and causal metadata are outside Core unless the selected residual
semantics explicitly commits to them.

## 5. Composition

Composition is an ordinary semantics, `capsule.compose@1`, whose residual may
reference child `ComputationRef` values, connections, and exports. A composed
result has the same `ComputationObject` shape as every other sealed
Computation. Core has no `Atomic | Composite` distinction.

The small-step model and residual invariants are developed in
[`CAPSULE_COMPOSE_SEMANTICS.md`](../../draft/CAPSULE_COMPOSE_SEMANTICS.md).

## 6. Core exclusions

State, Record, Interaction, Trace, Snapshot, Materialization, Connector, Run,
Evaluator, Session, calculus `Name`, `PortRef`, and wire/container formats are
outside the Semantic Core. New workloads are added by defining a versioned
semantics, its residual representation, and its evaluator/seal/resume
behavior—not by extending a Core category enum.

Protocol v1 is governed separately by
[`CAPSULE_PROTOCOL_V1_COMPATIBILITY.md`](CAPSULE_PROTOCOL_V1_COMPATIBILITY.md).
No portable Protocol v2 encoding is defined here.
