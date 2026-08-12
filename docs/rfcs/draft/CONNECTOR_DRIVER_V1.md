---
title: "Connector Driver Contract v1"
status: draft
date: 2026-08-12
author: "@egamikohsuke"
related:
  - "CONNECTOR_PROTOCOL_V1.md"
  - "CONNECTOR_DRIVER_STDIO_JSONRPC_V1.md"
  - "CAPSULE_SESSION_RUNTIME.md"
---

# Connector Driver Contract v1

## 1. Scope

This contract defines transport-independent semantics between an Ato Session
Supervisor and a Connector Driver. It does not define Capsule wire encoding or
require one Driver process per Session.

Drivers negotiate implementation identity, supported Connector Protocols,
attachment requirements, checkpoint compatibility, and capabilities at
initialization. Capabilities include offline replay, reconstruction,
checkpoint, fork, isolated/live operation, reconciliation, and an external
effect ceiling. The Capsule Descriptor records only the logical Protocol, not
these implementation capabilities.

## 2. Operations

Requests include `initialize`, `probe`, `prepare`, `connect`, `set_mode`,
`inject_ingress`, `expect_egress`, `deliver_ingress`, `begin_dispatch`,
`dispatch_allowed`, `reject_effect`, `quiesce`, `checkpoint`,
`restore_checkpoint`, `reconstruct`, `fork`, `reconcile_effect`, and `close`.

Notifications include `ingress_candidate`, `egress_candidate`,
`delivery_status`, `effect_status`, `quiesced`, `boundary_failure`, and
`health_changed`. A Driver never allocates Capsule Record `seq` values.

## 3. Boundary operation handshake

`BoundaryOperationId` is a local correlation identifier and is not portable
Capsule data.

Ingress remains buffered until the Supervisor durably commits it and sends
`deliver_ingress`. Egress and its optional effect intent are reported together
as one `egress_candidate`. An external effect cannot leave the boundary until
the Supervisor has durably entered `Dispatching` and sends
`dispatch_allowed`.

If a Driver fails while delivery status is uncertain, the current computation
incarnation is invalid. Recovery restarts all components from a durable common
frontier. The candidate MUST NOT be blindly delivered again to the same
incarnation.

## 4. Barriers and failures

`quiesce(barrier_id)` prevents new Ingress delivery, drains operations accepted
before the barrier, buffers or backpressures later external input, and returns
`quiesced` only at a protocol-defined safe cut.

Unexpected loss of an active Driver is a boundary failure. The Supervisor
immediately freezes computation. Same-incarnation reattachment is permitted
only when no boundary delivery or external effect is uncertain.

## 5. Discovery and trust

Drivers are resolved only through an explicitly populated, owner-scoped Driver
Registry. Ato MUST NOT search `PATH` or execute a local program merely because
a Capsule names an unknown Protocol. Unknown or untrusted implementations fail
closed. Third-party v1 Drivers are trusted local plugins; this contract does
not claim same-user sandboxing.
