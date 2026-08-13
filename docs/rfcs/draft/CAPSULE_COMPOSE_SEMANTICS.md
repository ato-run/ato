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
8. every child endpoint is linear and therefore appears in at most one
   connection or export, no endpoint is connected to itself, no connection has
   both endpoints on the same NodeId, and no undirected connection is
   duplicated; and
9. recursively referenced compose objects pass the same validation.

Role compatibility belongs to Protocol semantics. Compose receives an explicit
policy and must reject an unknown protocol or role pair; it does not infer
compatibility from role spelling.

### 3.1 Linear endpoint discipline

`capsule.compose@1` intentionally uses linear single-binding endpoints. A child
Port may participate in at most one connection or one parent export, never
both, and may not fan out across multiple connections. Fan-out, multiplexing,
and multiparty interaction require an explicit Hub, Router, or other ordinary
Computation whose Semantics defines that behavior. This keeps duplication and
arbitration out of the generic composition operator. A future non-linear
composition model must use a different versioned SemanticsId rather than
silently weakening `capsule.compose@1`.

### 3.2 Validation resource limits

Transitive validation operates over untrusted content and must be iterative,
not recursive. Each invocation has an explicit `ValidationBudget` containing:

```text
max_depth
max_unique_computations
max_resolved_bytes
```

The unique-computation count includes leaf and compose children, while the byte
budget includes canonical Computation Object bytes and distinct compose
residual bytes. A `ComputationRef` is resolved at most once per validation and
the verified result is reused for every Node occurrence. Exceeding a budget is
reported as `validation resource limit exceeded`, not semantic invalidity;
callers may retry under a larger trusted policy.

## 4. Graph policy

Cycles in the node-level connection graph are allowed. They describe feedback
between child Computations and do not imply recursive object identity.

Cycles in the transitive Computation reference graph are rejected. Reusing one
immutable child Computation from multiple nodes is allowed and validators
memoize each resolved reference, but a reference already on the current
iterative traversal path is invalid. Thus validation proves a finite, bounded
resolved object-graph closure without forbidding communication topologies.

## 5. Structural boundary visibility

An endpoint listed in `exports` is structurally visible at the mapped parent
Port. An endpoint used by a connection is structurally internal to the parent.
This version validates topology only; it does not observe child transitions or
claim that synchronization has occurred. `BoundaryVisibility::Internal` is a
structural fact, whereas `tau` below is an actual transition label.

## 6. Small-step reduction

The compose crate accepts verified child transition evidence, but does not
dispatch or evaluate child Semantics. For generic value type `V`, a label is:

```text
alpha ::= tau | p ? V | p ! V
```

`StepLabel<V>` is semantic transition metadata, not a Record, Trace, or wire
format; it has no serialization requirement. A `NodeStep<V>` names a node and
contains verified `from` and `to` `ResolvedComputation` values. This proves
their canonical bytes and digest correspondence, while the truth of the child
transition remains the child Semantics' responsibility.

The reducer accepts a `ValidatedComposite`, so a semantic transition always
starts from a structurally valid Composite. It produces a candidate
`CompositeResidual`, which callers must seal and validate before treating it
as a valid successor Computation:

```text
valid Composite -> transition -> candidate residual -> seal -> validate -> valid successor
```

### 6.1 Internal child step

```text
Ci --tau--> Ci'

-----------------------------
Compose(... Ci ...) --tau--> Compose(... Ci' ...)
```

The supplied `from` reference must equal the current node reference.

### 6.2 Connected synchronization

```text
Ci --p!v--> Ci'     Cj --q?v--> Cj'     (i,p) <-> (j,q)

------------------------------------------------
Compose(... Ci, Cj ...) --tau--> Compose(... Ci', Cj' ...)
```

The actions must be complementary, have equal values, and name exactly the two
endpoints of one declared connection. v1 rejects connections whose endpoints
share a NodeId (`SameNodeConnectionUnsupported`): synchronizing two ports of
one child would require multi-action child semantics and is outside this model.

### 6.3 Exported action

```text
Ci --p!v--> Ci'      export x = (i,p)

-----------------------------------
Compose(... Ci ...) --x!v--> Compose(... Ci' ...)
```

Input is symmetric. An unexported or internally connected child port cannot be
lifted as a parent action.

## 7. Hello World conformance scenario

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

The structural conformance scenario remains valid as written. Behavioral
conformance extends `NameProvider` with an externally exported `input` Port and
proves this evolution using test-only leaf semantics:

```text
C0 --name?"Bob"---------------> C1
C1 --tau----------------------> C2
C2 --greeting!"Hello, Bob!"---> C3
```

Each successor is canonical-encoded, content-addressed, sealed into an
ordinary `ComputationObject`, and revalidated. Their ComputationRefs differ,
while all four boundaries remain exactly `{ name, greeting }`. The first step
lifts the exported input from `NameProvider.input`; the second synchronizes
`NameProvider.name` with `Greeter.name` and preserves `Bob`; the third lifts
the exported output from `Greeter.greeting`.

### Draft PR acceptance criterion

Without changing Capsule Core, the test-only `Greeter + NameProvider` scenario
must execute, reseal, and revalidate:

```text
C0 --name?"Bob"--> C1 --tau--> C2 --greeting!"Hello, Bob!"--> C3
```
