---
title: "Capsule Compose Semantics"
status: draft
date: 2026-08-13
author: "@egamikohsuke"
related:
  - "../accepted/protocol/CAPSULE_PROTOCOL_SEMANTIC_CORE.md"
  - "../accepted/protocol/CAPSULE_COMPUTATION_OBJECT_V1.md"
---

# Capsule Compose Semantics

## 1. Scope

`capsule.compose@1` is an ordinary Computation semantics. This draft defines
the minimum residual model and transition invariants needed to test that the
Core remains closed under composition. It does not add a Composite variant to
Core and does not yet fix a canonical residual wire format.

## 2. Identity layers

Let `r` range over calculus `Name` values. A Name is a mobile runtime channel,
not a persistent `PortRef`.

```text
C = <P, Gamma>
Gamma : PortId -> (r, ProtocolId, RoleId)
```

For a sealed computation, Core stores only `PortId -> (ProtocolId, RoleId)`.
The `PortId -> r` realization remains in the semantics-specific residual.
`PortRef(ComputationRef, PortId)` is a persistent logical reference defined by
the runtime/persistence layer; evolution to a later ComputationRef does not
retarget an earlier PortRef.

## 3. Conceptual residual

```text
CompositeResidual {
  nodes:       NodeId -> ComputationRef
  connections: [Connection]
  exports:     PortId -> Endpoint(NodeId, PortId)
}
```

This is a semantic schema, not a Core Rust type. A future canonical residual
specification must define NodeId, ordering, duplicate rejection, role
compatibility evidence, and object-graph closure before producers interoperate.

## 4. Validation invariants

A sealed compose object is valid only when:

1. `exports.keys` exactly equals the parent `ComputationObject.boundary.keys`.
2. Every exported or connected endpoint names an existing child boundary Port.
3. Both ends of a connection use the same `ProtocolId` and roles compatible
   under that protocol's semantics.
4. Each parent export has the same protocol and externally visible role as its
   referenced child endpoint, subject to protocol-defined role projection.

Validation is transitive over resolved child computation objects and therefore
requires a generic `ObjectResolver` plus the canonical computation codec.

## 5. Small-step behavior

An unconnected or exported child action may become an observable parent action
according to export mapping and protocol role semantics. A compatible action
across a connection synchronizes the two child residuals and is hidden from
the parent as `tau`:

```text
child_i --a--> child_i'
child_j --complement(a)--> child_j'
------------------------------------- connection
compose(children) --tau--> compose(children[i := i', j := j'])
```

Independent child steps interleave. A resulting runtime composite state may be
sealed again; sealing its children and compose residual yields the same generic
`ComputationObject { semantics, boundary, residual }` shape.

## 6. Open work

- define protocol-owned role compatibility and action complement rules;
- define canonical encoding and hash vectors for `CompositeResidual`;
- define cycle and recursive object-graph policy;
- define seal behavior for live Names and partial synchronization;
- prove or test boundary preservation and tau hiding across evolution.
