---
title: Ato PTY Connector Protocol v1
status: draft
protocol_id: ato.io.pty@1
---

# Ato PTY Connector Protocol v1

This document defines the logical terminal boundary used by Ato Capsule
Sessions. It specializes the draft Connector Protocol contract; it does not
add a Capsule Semantic Core primitive.

## Boundary ownership

One active `ato.io.pty@1` Connector owns the byte flow between one terminal
computation and its external terminal clients. The shell process and process
group are owned by the computation runtime, not by the Connector Driver.
Attach, detach, writer leases, authentication, and local escape processing are
Control Plane operations and MUST NOT become I/O Records.

## Records

| RecordKind | Direction | Payload |
|---|---|---|
| `stdin` | Ingress | raw bytes |
| `output` | Egress | raw bytes |
| `resize` | Ingress | `{ rows: u16, cols: u16 }` encoded by the Connector profile |
| `exit` | Egress | exit status and termination reason encoded by the Connector profile |

Payload bytes MUST NOT be decoded as UTF-8 or newline-normalized. Portable
export policy MUST inspect terminal records because command lines and output
can contain credentials or private data.

## Framing and replay

PTY read segmentation is not semantic. Recorded adjacent `output` payloads
form one ordered byte stream. A replay verifier MUST buffer actual PTY output
and consume exactly each recorded payload length before comparing it. It MUST
NOT compare an operating-system `read()` result to one Record boundary.

Historical `stdin` is injected in sequence order. Historical `output` is
verified against actual output and MUST NOT be journaled again as new Isolated
history. Any byte difference is `Diverged`; verified history may enter
Isolated continuation only after the recorded frontier is fully verified.

## Safe cuts

During a quiesce barrier the Driver stops releasing new Ingress, drains all
accepted boundary operations to their durable state, buffers newly arriving
external input, and reports `Quiesced` only when no partial logical operation
crosses the cut. Attach state and observer queues are not part of the cut.

## Runtime rules

- Exactly one interactive writer lease is permitted; zero or more read-only
  observers are permitted.
- Writer disconnect releases the lease. Detach never means shell exit.
- PTY output is drained even with no clients attached.
- Observer queues are bounded. A slow observer may be disconnected or lose
  display bytes, but the Session WAL MUST NOT lose the corresponding Record.
- The Driver MUST NOT allocate Capsule Record sequence numbers.
- The workload MUST stop when its Supervisor lease is lost.
- Native PTY handles are Supervisor-local and MUST NOT appear in Capsule wire
  data or this Connector Protocol.
