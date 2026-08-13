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
  - "CAPSULE_PROTOCOL_V1_COMPATIBILITY.md"
---

# Capsule Semantic Core

## 1. Authority

The only semantic primitive in the Capsule Core is an immutable,
content-addressed residual `Computation`. This specification supersedes the
five-element semantic model in
[`CAPSULE_V1_EXECUTION_MODEL_SPEC.md`](../../archived/CAPSULE_V1_EXECUTION_MODEL_SPEC.md).

## 2. Computation

```text
Computation = identity + body + boundary
```

A `ComputationRef` contains a versioned `ComputationTypeId` and the content
address of a `ComputationObject`. The addressed object contains both its
type-defined body reference and its boundary. Consequently, the same
`ComputationRef` always identifies the same continuation and the same open
boundary.

## 3. Boundary

A boundary is the computation's map of typed open Ports. A Port declares its
protocol, direction, and optional content-addressed configuration. A Port is a
structure inside a Computation, not an independent semantic primitive.

Physical endpoints, transports, file descriptors, runtime adapters, and
Connectors are outside the Core.

## 4. Evolution

Evolution is the semantic relation:

```text
C -> C'
```

It produces a new immutable residual Computation. Observations, histories,
records, traces, clocks, replay evidence, and causal metadata may explain an
evolution, but they are not part of the current Computation unless its
type-defined body explicitly commits to them.

## 5. Composition

Composition maps computations and validated Port wiring to another
Computation:

```text
Computation* -> Computation
```

Composition is represented as the ordinary computation type
`capsule.computation.compose@1`. Its body schema may define children, wires,
hidden Ports, and exported Ports. Composition is not a second Core primitive.

## 6. Core exclusions

State, Record, Trace, Snapshot, Materialization, Connector, Run, Evaluator,
Session, and wire/container formats are outside the Semantic Core. They may be
type-defined bodies, evidence, compatibility representations, or runtime
mechanisms without becoming peer semantic objects.

Protocol v1 is governed separately by
[`CAPSULE_PROTOCOL_V1_COMPATIBILITY.md`](CAPSULE_PROTOCOL_V1_COMPATIBILITY.md).
No portable Protocol v2 encoding is defined here.
