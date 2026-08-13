---
title: "Capsule Compose Semantics"
status: draft
date: 2026-08-13
author: "@egamikohsuke"
ssot:
  - "crates/capsule-compose/"
related:
  - "../accepted/protocol/CAPSULE_PROTOCOL_SEMANTIC_CORE.md"
  - "../accepted/protocol/CAPSULE_COMPUTATION_OBJECT_V1.md"
---

# Capsule Compose Semantics

## 1. Scope

`capsule.compose@1` is an ordinary Computation semantics. Its residual names
sealed child Computations, connects child Ports, and projects selected child
Ports as the parent boundary. A composed value uses the unchanged Core shape:

```text
ComputationObject {
  semantics: capsule.compose@1,
  boundary,
  residual
}
```

This specification does not introduce a Core `Composite` variant, a runtime
Evaluator, live `PortRef` or `PortGrant` values, Trace, Contract, Snapshot, or
materialization state.

## 2. Canonical residual v1

The object referenced by `ComputationObject.residual` is the RFC 8785 JCS
encoding of exactly this JSON shape:

```json
{
  "nodes": {
    "<NodeId>": "<ComputationRef>"
  },
  "connections": [
    {
      "first": { "node": "<NodeId>", "port": "<PortId>" },
      "second": { "node": "<NodeId>", "port": "<PortId>" }
    }
  ],
  "exports": {
    "<parent PortId>": { "node": "<NodeId>", "port": "<PortId>" }
  }
}
```

No field is optional and unknown fields are invalid. `NodeId` follows the Core
component identifier grammar and is local to one residual. Connections are
undirected: each connection stores its lexicographically smaller endpoint as
`first`, and the connection array is sorted lexicographically by
`(first, second)`. Decoders reject bytes which are not this exact canonical
representation.

The residual is limited to 1 MiB. Its `ContentRef` uses BLAKE3 over the exact
canonical bytes. The encoding is permanently selected by the versioned
`capsule.compose@1` Semantics; an incompatible residual uses a new SemanticsId
and must not reinterpret these bytes.

## 3. Validation

Validation resolves the residual and every referenced child through the generic
`ObjectResolver`. A compose object is semantically valid only when:

1. its SemanticsId is exactly `capsule.compose@1`;
2. the residual bytes match their `ContentRef`, size limit, and canonical form;
3. every node reference resolves to a verified Computation Object v1;
4. every exported or connected endpoint names an existing node and child Port;
5. `exports.keys` exactly equals the parent boundary keys;
6. each exported child Port has the same protocol as its parent Port and a role
   accepted by that protocol's export-projection policy;
7. both connection endpoints have the same protocol and roles accepted by that
   protocol's connection policy;
8. no endpoint is used by more than one connection or export, no endpoint is
   connected to itself, and no undirected connection is duplicated; and
9. recursively referenced compose objects pass the same validation.

Role compatibility belongs to Protocol semantics. Compose receives an explicit
policy and must reject an unknown protocol or role pair; it does not infer
compatibility from role spelling.

## 4. Graph policy

Cycles in the node-level connection graph are allowed. They describe feedback
between child Computations and do not imply recursive object identity.

Cycles in the transitive Computation reference graph are rejected. Reusing one
immutable child Computation from multiple nodes is allowed and validators may
memoize a successfully validated reference, but a reference already on the
current resolution stack is invalid. Thus validation proves a finite resolved
object-graph closure without forbidding communication topologies.

## 5. Boundary and observation

An action at a child endpoint listed in `exports` is visible at the mapped
parent Port, subject to Protocol semantics. A compatible pair of child actions
across a connection synchronizes and is hidden from the parent as `tau`:

```text
child_i --a--> child_i'
child_j --complement(a)--> child_j'
------------------------------------- connection
compose(children) --tau--> compose(children[i := i', j := j'])
```

The compose model exposes only this visibility classification. It does not
define a generic event history or Trace. Independent child steps and the
meaning of actions remain properties of the selected child and Protocol
semantics.

## 6. Hello World conformance scenario

The minimum conformance graph contains a `Greeter` child and a `NameProvider`
child. Their `name` Ports are connected and are not exported. The Greeter's
`greeting` Port is exported as the sole parent boundary Port.

```text
NameProvider.name <── internal connection ──> Greeter.name
                                                |
                                                └── Greeter.greeting
                                                        |
                                                        v
                                              parent boundary: greeting
```

Validation must produce one ordinary `ComputationObject`, preserve exactly the
external `greeting` Port, classify the connected `name` interaction as `tau`,
and do so without changing the Semantic Core.
