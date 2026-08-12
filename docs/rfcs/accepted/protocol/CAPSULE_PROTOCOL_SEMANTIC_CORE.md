---
title: "Capsule Protocol Semantic Core"
status: accepted
date: 2026-08-12
author: "@egamikohsuke"
supersedes:
  - "../../archived/CAPSULE_V1_EXECUTION_MODEL_SPEC.md"
ssot:
  - "crates/capsule-protocol/"
  - "CAPSULE_CBOR_V1.cddl"
  - "CAPSULE_BUNDLE_V1.md"
---

# Capsule Protocol Semantic Core

## 1. Authority

This specification supersedes the five-element semantic model in
[`CAPSULE_V1_EXECUTION_MODEL_SPEC.md`](../../archived/CAPSULE_V1_EXECUTION_MODEL_SPEC.md).
Existing manifests, execution contracts, and Ready-State artifacts remain
supported implementation inputs; they are no longer primitives of the Capsule
Protocol Semantic Core.

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

Historical replay advances State only as required to process the recorded I/O
sequence. An empty record stream is a zero-step replay: State is unchanged and
no Connector is invoked. Autonomous evolution after historical replay belongs
to continuation, not historical replay. A State type that requires an implicit
clock, scheduler step, or other influence to reproduce history must model that
influence as recorded I/O rather than adding another Core primitive.

## 4. Domain and encoding boundary

`capsule-protocol` defines validated semantic values and incremental stream
validation. It contains no serializer, filesystem, network, async runtime, or
workspace-crate dependency.

Portable encoding is a separate layer. Semantic records are not encoded frames;
wire DTOs must convert explicitly to and from domain values. Large payloads use
algorithm-tagged content references rather than unbounded inline bytes.

All component identifiers use lowercase ASCII namespace segments separated by
`.`. A segment starts and ends with a lowercase letter or digit and may contain
`-` or `_` internally. Versioned identifiers contain at least two segments and
end in `@<positive-decimal-version>` without a leading zero. The 255-byte limit
applies to the complete identifier, including `@version`. Descriptor connector
entries are encoded in strictly ascending `ConnectorId` byte order.

The normative v1 wire schema is `CAPSULE_CBOR_V1.cddl`. A descriptor is one
CBOR item and its record stream is an RFC 8742 CBOR Sequence. Tuple positions,
null semantics, numeric bounds, payload tags, and `ContentRef` spelling are
fixed there. Exact tuple arity is required: decoders reject unknown/trailing
fields. Golden vectors under `crates/capsule-codec/tests/vectors/` test this
schema but are not the specification.

The normative portable container is `CAPSULE_BUNDLE_V1.md`. It defines how the
descriptor, record sequence, and content-addressed objects are carried in one
deterministic `.capsule` file. Bundle versioning is independent from CBOR wire
versioning.

`ato.state.workspace-posix-host@1` is explicitly host-bound and best-effort. It
owns workspace bytes only; host shell, runtime, toolchain, libraries, `PATH`,
and environment are outside its continuation guarantee. It MUST NOT be used as
evidence of cross-environment portability. Portable acceptance requires a
State type such as `ato.state.ready-state@1` whose restore contract owns every
replay-relevant runtime influence, or an equivalently pinned Ato-managed
runtime.

CI's normative portability acceptance uses the Ato-managed
`ato.state.fixture-machine@1` runtime in two separate jobs. Its versioned State
semantics close computation inside Ato, the only transferred input is the
validated `.capsule`, replay compares actual egress, and the restored machine
accepts new input after replay. The PTY/rustc test remains a separate
host-bound integration smoke and makes no cross-environment portability claim.

## 5. Compatibility surface

The initial schema version is 1. Valid streams may be empty, start at any
sequence number, contain sequence gaps, omit pacing/audit timestamps, and have
wall-clock timestamps that move backwards. Sequence numbers must be strictly
increasing and every record must name a declared connector.

Build, Runtime, Launch, Snapshot, Materialization, Binding, Surface, Ledger,
Cursor, Contract, and Lineage may exist as implementation strategies or derived
views. They are not Semantic Core primitives.
