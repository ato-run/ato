---
title: "Capsule Protocol v1 Compatibility"
status: accepted
date: 2026-08-13
author: "@egamikohsuke"
related:
  - "CAPSULE_PROTOCOL_SEMANTIC_CORE.md"
  - "CAPSULE_CBOR_V1.cddl"
  - "CAPSULE_BUNDLE_V1.md"
ssot:
  - "crates/capsule-protocol/"
  - "crates/capsule-codec/"
  - "crates/capsule-compat-v1/"
---

# Capsule Protocol v1 Compatibility

## 1. Authority and representation

Protocol v1 is the accepted legacy representation:

```text
Protocol v1 Capsule
├── State
├── Connector definitions
└── I/O Record stream
```

These are normative Protocol v1 values, not native Semantic Core primitives.
The governing v1 invariant is:

> State type defines continuation semantics. Every replay-relevant influence
> not owned by State must cross an I/O Connector. Uncaptured influence is
> outside the replay guarantee.

`capsule-protocol` owns this exact domain. It does not define Computation,
native interaction history, runtime materialization, or Protocol v2.

## 2. State, Connector, and Record

State is Capsule-owned computation state plus a versioned state type. The
state type owns restore and continuation semantics. Ready-State may therefore
use `ato.state.ready-state@1` while Firecracker, QEMU, and fake backends remain
implementation details.

An I/O Connector is a versioned boundary transducer between State and the
external world. Its adapter owns payload interpretation and record/live/
sandbox/redirect/blocked policy. An I/O Record is canonical ingress or egress
observed at that Connector. `seq` is recorder-established serialization order;
wall time is audit metadata and monotonic offset is pacing metadata.

Valid streams may be empty, start at any sequence number, contain gaps, omit
timestamps, and have wall-clock timestamps that move backwards. Sequence
numbers must be strictly increasing and every Record must name a declared
Connector.

## 3. Replay

Ingress and egress are asymmetric:

```text
recorded ingress -> inject into restored State
actual egress    -> observe and compare with recorded egress
```

Recorded egress must never be injected. Empty history is zero-step replay:
State is unchanged and no Connector is invoked. Autonomous work after history
belongs to continuation, not historical replay. An implicit clock, scheduler,
or other influence required to reproduce history must be owned by State or
recorded through a Connector.

## 4. Computation adapter

`capsule-compat-v1` adapts a validated Bundle v1 to one opaque computation type:

```text
Protocol v1 Bundle
  -> capsule.computation.legacy-v1@1
  -> content-addressed ComputationObject
  -> ComputationRef
```

The type-defined legacy body contains content references to the exact v1
descriptor and Record Sequence bytes. It does not translate State, Connector,
or `IoRecord` into native Core objects. Only the externally composable boundary
is projected: each v1 Connector becomes a duplex compatibility Port preserving
its protocol and configuration reference.

The `ComputationObject` contains that boundary and the legacy body reference,
so changing a projected Port necessarily changes the `ComputationRef`.
Connector identity equals Port identity only inside this explicit v1
projection; native binding requires a separate `PortId -> ConnectorId` map.

## 5. Import safety

Normalization never rewrites its input Bundle. Descriptor and Record members
are copied into an owner-only CAS in bounded chunks while computing BLAKE3 and
verifying copied byte count against the Bundle-validated member size. Existing
objects are validated by streaming size and hash. CAS publication is atomic,
content-verified, idempotent, and fails closed on conflict.

The same exact v1 members and projected boundary produce the same
`ComputationRef`. Existing objects referenced by the v1 members retain their
v1 integrity and interpretation rules.

## 6. Run compatibility

A generic Run stores its immutable `origin_computation`. Protocol v1-specific
Record frontiers, historical replay verification, Connector checkpoints,
State checkpoints, and workspace/Ready-State materializations live only inside
the `legacy_v1` runtime profile. A checkpoint is runtime recovery data and must
never replace the origin Computation.

Session Store v2/v3 State identities and the earlier v4 layout remain readable.
After the seed Bundle is normalized they are upgraded to a native
`ComputationRef`; new v4 writes require that native origin.

Evaluator registration is exact by `ComputationTypeId`. The evaluator consumes
a hash-validated `ResolvedComputation`, whose boundary is already inside its
object, plus an `EvaluationContext` containing an object resolver, explicit
Port bindings, Session paths, and materialization services. Routing existing
Workspace PTY and Ready-State startup through this registry is a later change.

## 7. Wire and container stability

The normative wire schema is
[`CAPSULE_CBOR_V1.cddl`](CAPSULE_CBOR_V1.cddl). The normative container is
[`CAPSULE_BUNDLE_V1.md`](CAPSULE_BUNDLE_V1.md). Exact tuple arity, null
semantics, numeric bounds, payload tags, identifier rules, descriptor ordering,
Bundle member layout, closure, integrity, and limits remain defined there.

This semantic separation changes no CBOR v1 or Bundle v1 byte. Native
interaction format, Protocol v2, generic replay, computation seal, and
composition evaluation are outside this compatibility milestone.
