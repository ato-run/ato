---
title: "Ato Capsule Session Runtime"
status: draft
date: 2026-08-12
author: "@egamikohsuke"
related:
  - "../accepted/protocol/CAPSULE_PROTOCOL_SEMANTIC_CORE.md"
  - "CONNECTOR_PROTOCOL_V1.md"
  - "CONNECTOR_DRIVER_V1.md"
---

# Ato Capsule Session Runtime

## 1. Normative invariants

1. Every durable checkpoint is a consistent cut.
2. Every branch restores all components to the same recovery frontier `C` and
   replays exactly `(C, F]`.
3. Every Ingress becomes durable before delivery.
4. Every external effect enters durable `Dispatching` before leaving the
   Connector boundary.
5. Uncertain boundary delivery invalidates the live runtime incarnation.
6. An uncertain external effect blocks the Session until reconciliation proves
   its outcome.
7. Supervisor loss never allows computation to advance outside the journaled
   boundary.
8. Isolated execution is ordinary execution with all boundary I/O journaled.
9. `encap` exports only a committed consistent frontier.
10. Branch, Checkpoint, Effect, and Session remain runtime concepts and never
    become Semantic Core primitives.

Additionally, a Record MUST NOT be replayed against State or Connector state
that already contains the effect of that Record.

## 2. Session model

Lifecycle, Historical Replay verdict, and per-Connector mode are independent.
Lifecycle distinguishes clean `Suspended`, recoverable safety `Blocked`, and
terminal `Failed`. A verified Replay verdict records its exact `from` and
`through` frontier. Connector modes are Historical Replay, Isolated, Live, and
Blocked. `RecordFrontier` is either `Origin` or `Through(seq)`.

The runtime also tracks a non-portable `JournalLsn`. A committed cut is
represented by `DurableFrontier { records_through, journal_through }`; the
Record frontier is therefore evidence-backed by one exact durable WAL
position rather than supplied by a caller.

Isolated execution has no special recording mode: all boundary I/O is always
journaled. Portable publication is an explicit `encap` operation.

## 3. Common recovery and branching

For source frontier `F`, the runtime chooses the newest common recovery
frontier `C <= F` at which State and every Connector can be restored. Each
Connector uses Fresh, compatible Checkpoint, or reconstruction-from-Records.
After every component is at `C`, Replay applies exactly `(C, F]` once.
Fresh recovery is valid only for a Connector that explicitly declares itself
frontier-independent. Reconstruction start MUST NOT be after `C`.

A History-preserving Capsule stores an earlier State plus strictly later
Records. A Rebased Capsule stores frontier State plus only Records strictly
after that frontier. Head State MUST NOT be bundled with Records whose effects
it already contains.

## 4. Consistent frontier barrier

Branch, Checkpoint, Suspend, and capture-latest export use a barrier. Drivers
stop new delivery, drain pre-barrier work, buffer or backpressure later input,
and report the same barrier only at a Connector Protocol safe cut. Computation
is paused, final candidates and the local journal become durable, and State and
Connector checkpoints are committed in one frontier manifest. A timeout or
frontier mismatch rejects the entire operation.

The barrier API MUST NOT accept a caller-selected Record frontier. After all
Drivers reach their safe cut, the Supervisor reads the authoritative
`DurableFrontier` from the Session WAL. Each Driver independently reports the
Record frontier it reached; the Supervisor compares every report with the
WAL-backed frontier. A Driver is never given an expected frontier merely to
echo it.

## 5. Local journal and effect transactions

The local journal is a recoverable WAL distinct from portable CBOR Sequence.
An operation becomes durably committed before delivery or external dispatch is
released. Group commit is permitted; one Record does not imply one `fsync`.
Recovery accepts complete committed frames and may discard only an incomplete
EOF tail. A complete frame with invalid magic, length, checksum, commit marker,
or body is committed-journal corruption and recovery MUST fail closed; it MUST
NOT silently roll history back. Sequence gaps are valid, but a previously
allocated sequence number is never reused for different content.

`BoundaryOperationId` uniqueness is checked or atomically reserved before its
candidate is appended. The WAL preserves this uniqueness across process
recovery. A duplicate operation MUST NOT add another WAL frame.
Runtime memory transitions are applied only after the corresponding WAL state
transition commits, so a failed commit leaves memory at the last durable state.

When recovery finds an incomplete EOF tail, `SessionWal::open` truncates the
physical file to `DurableFrontier.journal_through` and durably commits that
repair before permitting another append. A repaired WAL therefore remains
recoverable after later valid frames are added.

The WAL owns sequence allocation authority. A new Record candidate MUST have a
`seq` strictly greater than the durable allocated high-water mark. Sequence
gaps are valid. High-water marks are monotonic non-decreasing, and the mark
written immediately after a candidate may equal that candidate's `seq`.
Append and recovery both enforce these rules; reuse or regression fails closed.

External effect states are Prepared, Authorized, Dispatching, optionally
Dispatched, Completed, InDoubt, Reconciled, and Rejected. `Dispatching` is
committed before dispatch permission. A crash from Dispatching until Completed
produces InDoubt. It is never blindly retried; reconciliation or an idempotency
contract must prove its outcome, otherwise the Session remains Blocked.

## 6. Attachment and containment

Drivers declare attachment requirements before restore. The Supervisor creates
an Attachment Plan; the State Runtime materializes environment, sockets,
namespaces, or vsock endpoints and restores computation paused. Drivers connect
to the concrete endpoints, and computation resumes only after every boundary
is ready.

Driver Connector IDs and attachment requirement IDs MUST match and be unique
before any Driver is prepared or State restore begins. Duplicate active
Connectors fail closed rather than using map insertion order.

Supervisor or active-Driver loss freezes or terminates computation. A watchdog,
parent-death facility, heartbeat lease, or VMM control enforces this invariant.
Recovery does not trust an orphan incarnation; it reconstructs from the latest
durable common frontier. Same-incarnation Driver reattachment is allowed only
when no delivery or external effect is uncertain.

Bootstrap failure closes every Driver already prepared or connected and
terminates any restored computation. Barrier failure invalidates the whole
runtime incarnation: all Drivers are closed, computation is terminated, and
recovery starts from a durable common frontier. Driver `close` is idempotent.

Session checkpoint manifests persist `captured_at: DurableFrontier`, and local
Session records persist the complete committed `DurableFrontier`. Connector
checkpoint consistency compares `applied_through` with
`captured_at.records_through`. This distinguishes multiple durable WAL cuts at
the same Record frontier, including effect transaction metadata transitions.

## 7. Portable export

`encap` exports either an explicitly selected committed frontier or creates a
new committed frontier through a barrier. It never reads an uncommitted live
head. The exported State provides its complete object closure. Portable policy
covers State objects, Connector configuration, and inline and object-backed I/O
payloads. Portable eligibility is evaluated dynamically at capture time.

Local checkpoints and Connector checkpoint objects are not Portable Capsules.

## 8. Reference implementation boundary

One local Supervisor OS process per Session, owner-only UDS or current-user
Windows named pipe, session secret, Supervisor generation, incarnation nonce,
PID, and process start identity define the initial reference implementation.
They are not requirements on managed multi-Session runners.

The foundation Session Store and Driver Registry currently implement
owner-only filesystem storage on Unix. Until a Windows ACL backend is added,
opening either store on Windows MUST fail closed with an unsupported-platform
error; this draft does not claim Windows owner-only filesystem storage yet.
