---
title: "Connector Protocol v1"
status: draft
date: 2026-08-12
author: "@egamikohsuke"
related:
  - "../accepted/protocol/CAPSULE_PROTOCOL_SEMANTIC_CORE.md"
  - "CONNECTOR_DRIVER_V1.md"
  - "CAPSULE_SESSION_RUNTIME.md"
---

# Connector Protocol v1

## 1. Scope

A Connector Protocol specifies the logical meaning of one boundary between
Capsule-owned computation and the outside world. It extends, but does not add
primitives to, the Capsule Protocol Semantic Core.

Each versioned Connector Protocol specification MUST define:

- its `ProtocolId` and namespace ownership;
- the external information flow owned by the boundary;
- every `RecordKind`, its `Direction`, and payload schema;
- logical framing and ordering semantics;
- Historical Replay injection and verification behavior;
- safe-cut and quiesce-barrier semantics; and
- privacy, redaction, and portable-export policy.

`ato.*` is reserved for Ato specifications. Third-party identifiers use a
reverse-domain namespace.

## 2. Boundary ownership

> One external information flow MUST be owned by no more than one active
> Connector during a Session.

When HTTP owns a flow, its TLS, TCP, and DNS traffic are Driver implementation
details and MUST NOT simultaneously be captured by other active Connectors.
An implementation may choose a lower boundary, such as TCP, when no higher
logical protocol is available.

## 3. Replay and safe cuts

Recorded Ingress is injected into computation. Actual Egress is observed and
verified against recorded Egress; recorded Egress is never injected as a fake
computation result.

A Connector Protocol MUST state when its logical conversation admits a safe
cut. A Driver MUST NOT complete a quiesce barrier while an operation, frame, or
transaction is at a protocol-defined unsafe cut. It may finish the operation,
buffer it without delivery, or apply backpressure. Barrier timeout fails the
frontier operation.

## 4. Portable data

The protocol specification identifies credentials, private data, and other
payload fields that require rejection or redaction during portable export.
This policy applies to inline records, object-backed records, and Connector
configuration objects. Local checkpoints are outside portable export.
