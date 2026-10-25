# Asynchronous Record Writer and RecordFrontier

Status: Draft

## Invariant

Computation execution must not wait for ordinary Record persistence. The v2
Stylus path performs in-memory structural validation and one non-blocking send
to a bounded queue. It does not wait for CAS insertion, filesystem creation,
`fsync`, network upload, SQLite, or ComputationRef advancement.

```text
Stylus
  -> RecordCandidate
  -> bounded sync_channel::try_send

Record Writer thread
  -> operation payload schema validation
  -> payload CAS put
  -> writer_order assignment
  -> canonical RecordEnvelopeV2 encoding
  -> append active.open
  -> seal immutable segment
  -> publish processed watermark

Capture Barrier only
  -> pause Stylus admission
  -> snapshot accepted-through watermarks
  -> wait for Writer drain
  -> fsync and seal active segment
  -> verify causal cut and payload closure
  -> seal RecordFrontier
```

## Overflow policy

The queue is bounded. All built-in v2 Protocol operations use the
drop-forbidden policy: a full queue returns an explicit error and marks the Run
failed. A Record is never silently discarded, memory is never allowed to grow
without a bound, and overflow does not fall back to synchronous persistence.

Third-party Protocol registration uses the same policy in v1 of this API. A
future Protocol-specific backpressure policy must remain explicit and cannot
select silent drop.

## Payload validation

`RecordSchemaRegistry` is extension-owned and keyed by Protocol, operation, and
payload version. Required features must be a subset of the registered
operation features. The built-in registry checks the exact variant for HTTP
request, Browser keyboard/pointer/click/scroll, PTY input/resize/signal,
Workspace put/delete/rename, Binding attach/replace/detach, and Adapter
add/remove/configure.

Application-generated HTTP responses and PTY output have no registered Record
schema and do not enter the Stylus path.

## Segment store

The run-scoped layout is:

```text
.capsule/records/runs/<run>/
├── active.open
├── segments/
│   └── <blake3>.seg
└── frontiers/
    └── <blake3>.json
```

`active.open` is an append-only sequence of length-delimited canonical v2
Records. The writer alone assigns a positive global `writer_order`. A sealed
segment has a versioned magic, Record count, first/last writer order, exact
byte length, sorted unique payload closure, and canonical Record frames. The
digest covers the complete immutable segment bytes. Seal and load both verify:

- contiguous writer order;
- count and first/last bounds;
- exact byte length;
- unique sorted payload closure derived from Records;
- every payload object exists;
- canonical Record encoding and Record identity;
- digest-derived immutable path.

Segments and frontier identity bodies are also inserted into the local CAS for
later object-graph transport. Files in the run directory remain the
append/seal authority. SQLite, if added as a projection, is not the Record
source of truth or durability boundary.

## Capture Barrier

`pause_and_seal` serializes admission with Stylus submission, marks admission
paused, snapshots accepted-through watermarks, queues a barrier after all
earlier candidates, and waits for the Writer. The returned lease keeps Stylus
admission paused while a caller quiesces or captures physical resources. Drop
releases the pause.

Stop uses this order:

```text
Capture Barrier / causal cut
-> keep Stylus paused
-> Adapter or Realization quiesce
-> persist run-to-frontier association
-> acknowledge Stop
```

This prevents a Record observed after the cut from being mistaken as part of
the capture.

## RecordFrontier

The canonical frontier contains:

- version and run ID;
- ordered sealed segment summaries;
- last writer order;
- per-Stylus observed-through watermarks;
- causal cut (unreferenced Record tips);
- a BLAKE3 identity over the canonical body.

Loading a frontier traverses every segment and recomputes writer order,
payload closure, watermarks, and causal cut. A cached descriptor cannot replace
this closure validation.

The Record Writer generates only Record IDs, segment digests, and
RecordFrontier identity. It has no ComputationRef input and never updates a Run
head. The supervisor stores a separate, higher-level association from an
explicitly sealed Run target to its RecordFrontier. `ato.replay@2` carries the
frontier reference as provenance; it does not derive its target from it.

## Compatibility

Legacy `ato.replay@1` authoring can continue using per-Record files and head
chaining while old bundles migrate. A configuration selecting
`ato.replay@2` routes ordinary operations only through the asynchronous writer
and leaves the legacy observation/head path empty. New v2 encap resolves its
Record closure from the sealed frontier.

## Non-goals

- deriving ComputationRef from Record order, Record payload, segment, or
  RecordFrontier;
- storing output, evidence, logs, screenshots, or diagnostics as Records;
- silently dropping on pressure;
- using SQLite as the Record authority;
- making a Capture Barrier part of every ordinary operation.
