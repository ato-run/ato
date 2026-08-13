---
title: "Capsule Computation Compatibility Layer"
status: draft
date: 2026-08-13
author: "@egamikohsuke"
related:
  - "../accepted/protocol/CAPSULE_PROTOCOL_SEMANTIC_CORE.md"
  - "../accepted/protocol/CAPSULE_BUNDLE_V1.md"
  - "CAPSULE_SESSION_RUNTIME.md"
---

# Capsule Computation Compatibility Layer

## 1. Purpose

The native semantic identity of a Capsule is an immutable, content-addressed
residual `Computation`. `State`, Connector definitions, and I/O Records remain
the exact Capsule Protocol v1 compatibility model; they are not silently
reinterpreted as Protocol v2 values.

This RFC introduces the internal compatibility boundary required before a
portable Protocol v2 is defined:

```text
Bundle v1 (State + Connectors + Records)
  -> LegacyStateIoComputationV1
  -> ComputationRef + Ports
  -> evaluator-selected Run
```

The accepted v1 CBOR and Bundle specifications remain unchanged.

## 2. Native domain

`ComputationRef` consists of a versioned `ComputationTypeId` and a
`ContentRef`. A `ComputationDescriptor` identifies the current root, its open
typed Ports, and optional trace evidence from an earlier computation.

A Port declares a `ProtocolId`, an optional content-addressed configuration,
and one of `IngressOnly`, `EgressOnly`, or `Duplex`. Physical endpoints,
transports, file descriptors, hostnames, and device identities MUST NOT enter a
Port definition.

An `InteractionRecord` names a Port rather than a Connector. Sequence numbers
are strictly increasing. Direction MUST be allowed by the declared Port mode.

## 3. v1 compatibility object

The compatibility computation type is:

```text
capsule.computation.legacy-state-io@1
```

Its canonical object is:

```text
{
  schema: "capsule.computation.legacy-state-io.object@1",
  descriptor_ref: ContentRef,
  record_stream_ref: ContentRef
}
```

The object is canonicalized with JCS and content-addressed with BLAKE3. The
exact accepted v1 descriptor and Record CBOR Sequence members are independently
stored in a session-local owner-only CAS. Existing objects referenced by those
members retain their v1 meaning and integrity requirements.

The resulting computation means: restore the v1 base State, attach the declared
Connectors, and apply the v1 Record stream exactly once according to the
accepted v1 replay contract. The continuation after that recipe is the sealed
legacy computation.

## 4. Connector projection

Each v1 `ConnectorId` is projected one-for-one to a `PortId`; both identifiers
use the same syntax. `ProtocolId` and `config_ref` are preserved.

Protocol v1 has no direction declaration at the Connector definition, so its
compatibility Port MUST be `Duplex`. Narrower native Port modes may only be
declared by a native computation descriptor.

Connector remains the runtime adapter that binds a semantic Port to a physical
endpoint. Projection does not make Connector and Port identical.

## 5. Safety and compatibility invariants

1. v1 tuple arity, member layout, bytes, and validation rules do not change.
2. Normalization never mutates or rewrites the input Bundle.
3. The same exact v1 members produce the same compatibility ComputationRef.
4. CAS writes are owner-only, atomic, content-verified, and idempotent.
5. A conflicting object at an existing content address fails closed.
6. Native interaction validation rejects undefined Ports, non-increasing
   sequence numbers, and direction/Port-mode mismatches.
7. Ready-State containment and State-specific restoration remain runtime
   materialization policies below the computation evaluator boundary.

## 6. Migration order

1. Land this native domain and v1 projection while preserving v1 golden bytes.
2. Route workspace and Ready-State startup through the exact-type
   `ComputationEvaluatorRegistry`. Evaluators receive semantic Ports and a
   per-Run `PortBindingPlan`; existing State runtimes, snapshot backends, and
   attachment endpoints remain materialization mechanisms below that boundary.
3. Persist computation origin and lazy Run heads in Session Store v4.
4. Define an unambiguous dual-reader Protocol and Bundle v2.
5. Add native atomic computation capture/seal.
6. Add validated composition, wiring, hiding, and export, with Hello World as
   the conformance fixture.

Protocol v2 MUST NOT precede the internal evaluator migration.
