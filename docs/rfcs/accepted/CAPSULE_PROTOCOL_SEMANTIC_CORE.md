---
title: "Capsule Protocol Semantic Core"
status: accepted
date: 2026-08-12
author: "@egamikohsuke"
supersedes:
  - "CAPSULE_V1_EXECUTION_MODEL_SPEC.md"
ssot:
  - "crates/capsule-protocol/"
  - "CAPSULE_CBOR_V1.cddl"
---

# Capsule Protocol Semantic Core

## 1. Authority

This specification supersedes the five-element semantic model in
`CAPSULE_V1_EXECUTION_MODEL_SPEC.md`. Existing manifests, execution contracts,
and Ready-State artifacts remain supported implementation inputs; they are no
longer primitives of the Capsule Protocol Semantic Core.

## 2. Core

A Capsule has exactly these semantic elements:

```text
Capsule
├── State
└── I/O
    ├── Connector definitions
    └── Record stream
```

The governing invariant is:

> **State type defines the continuation semantics. Every replay-relevant
> influence not owned by State must cross an I/O Connector. Uncaptured
> influence is outside the replay guarantee.**

State is Capsule-owned computation state plus a versioned state type. The state
type identifies the continuation semantics; the Core does not prescribe how a
runtime restores or captures it. A Ready-State manifest can therefore be
referenced as `ato.state.ready-state@1` while Firecracker, QEMU, and fake
backends remain physical implementation details.

An I/O Connector is a versioned boundary transducer between Capsule-owned state
and the external world. Connector adapters own payload interpretation and any
record/live/sandbox/redirect/blocked policy. Terminal, HTTP, browser, and future
record kinds do not become Core enum variants.

An I/O Record is canonical ingress or egress observed at that boundary. `seq`
is the recorder-established serialization order. Wall time is audit metadata;
monotonic offset is pacing metadata. Neither changes order.

## 3. Replay semantics

Ingress and egress are intentionally asymmetric:

```text
recorded ingress -> inject into computation
actual egress    -> observe and compare with recorded egress
```

Recorded egress must never be injected as if computation produced it. A replay
may report divergence when actual and recorded observations disagree. Replay
may leave the connector attached and return control to a user, producing
interactive continuation without introducing another semantic primitive.

No I/O does not imply no state transition. It means only that, without
unrecorded external information, evolution is closed under the state type's
execution semantics.

## 4. Domain and encoding boundary

`capsule-protocol` defines validated semantic values and incremental stream
validation. It contains no serializer, filesystem, network, async runtime, or
workspace-crate dependency.

Portable encoding is a separate layer. Semantic records are not encoded frames;
wire DTOs must convert explicitly to and from domain values. Large payloads use
algorithm-tagged content references rather than unbounded inline bytes.

The normative v1 wire schema is `CAPSULE_CBOR_V1.cddl`. A descriptor is one
CBOR item and its record stream is an RFC 8742 CBOR Sequence. Tuple positions,
null semantics, numeric bounds, payload tags, and `ContentRef` spelling are
fixed there. Exact tuple arity is required: decoders reject unknown/trailing
fields. Golden vectors under `crates/capsule-codec/tests/vectors/` test this
schema but are not the specification.

`ato.state.workspace-posix-host@1` is explicitly host-bound and best-effort. It
owns workspace bytes only; host shell, runtime, toolchain, libraries, `PATH`,
and environment are outside its continuation guarantee. It MUST NOT be used as
evidence of cross-environment portability. Portable acceptance requires a
State type such as `ato.state.ready-state@1` whose restore contract owns every
replay-relevant runtime influence, or an equivalently pinned Ato-managed
runtime.

## 5. Compatibility surface

The initial schema version is 1. Valid streams may be empty, start at any
sequence number, contain sequence gaps, omit pacing/audit timestamps, and have
wall-clock timestamps that move backwards. Sequence numbers must be strictly
increasing and every record must name a declared connector.

Build, Runtime, Launch, Snapshot, Materialization, Binding, Surface, Ledger,
Cursor, Contract, and Lineage may exist as implementation strategies or derived
views. They are not Semantic Core primitives.
