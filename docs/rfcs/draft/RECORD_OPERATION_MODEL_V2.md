# Record Operation Model v2

Status: Draft

## Problem

The accepted Protocol/Adapter and Computation Architecture RFCs model an
adapter observation as a Record and advance a computation head for every
stored observation. That model makes terminal output, HTTP responses, and
other non-applicable evidence part of replay, couples recording implementation
identity to replay routing, and puts persistence on the computation execution
path.

This draft defines a new, versioned boundary. It does not silently change the
legacy `ato.replay@1` reader or legacy `RecordEnvelope`.

## Semantic boundary

A Record is a portable semantic operation that an Actuator can apply to a
Realization. Applicability is the only universal guarantee. It does not imply
success, deterministic replay, identical output, or memory equivalence.

```text
Stylus
  Environment / Realization -> RecordCandidate

Record Writer
  RecordCandidate -> canonical RecordEnvelopeV2

ActuatorProvider
  target Environment + operation requirement -> provisionable route

Actuator
  RecordEnvelopeV2 -> Environment / Realization

Player
  decode -> resolve route -> provision/bind -> apply -> propagate result
```

`recorded_by` is provenance. It is never the sole compatibility or route
identity. Compatibility is described by protocol, operation, payload version,
port, and required features. `OperationId` identifies an operation inside a
Protocol.

## RecordEnvelopeV2

The canonical envelope contains:

- Record identity;
- Protocol and operation identifiers;
- Port identifier;
- payload content reference and payload version;
- required features;
- optional recording implementation provenance;
- stream-local sequence and writer-assigned global order;
- causal Record references;
- observation timestamp.

It intentionally excludes `head_before`, `head_after`, and
`semantic_frontier`. Record identity is the BLAKE3 digest of the canonical JCS
body and is independent from Computation identity.

## Applicable operations only

The v2 Record path admits only operations that have an Actuator contract.

- HTTP records inbound requests. Application-generated responses are runtime
  output.
- Browser records canonical `keyboard`, `pointer`, `click`, and `scroll`
  operations from the Browser input boundary. Screenshots, DOM snapshots,
  console output, and presentation media remain outside the Record Store.
- PTY records input, resize, and signal. Output, attach/detach observations,
  stdout, and stderr are runtime logs.
- Workspace records put, delete, and rename while retaining boundary and CAS
  checks.
- Binding records applicable attach, replace, and detach operations without
  secret values.
- Process output, exit observations, metrics, screenshots, DOM dumps, and
  diagnostics remain outside the Record Store.

The built-in `ato.adapter@1` add, remove, and configure operations are ordinary
Records. Their payload semantics are implemented by an Actuator Provider, not
by Player Core.

## Player and preflight

Player validates structural payload shape and requires exactly one
deterministic, provisionable Actuator route for each Record. A port binding can
select a route from several installed providers. The existence of multiple
implementations is therefore not itself an ambiguity.

Player resolves each Record at application time and does not simulate earlier
Record effects. It does not infer that a later browser operation is invalid
from an earlier adapter removal. The selected Actuator reports that error when
the operation is applied.

`required_operations` in an `ato.replay@2` descriptor is a cached summary. The
reader derives `RequiredOperationSet` from the complete Record closure and
fails closed if the summary differs.

## Versioning and migration

- `ato.replay@1` and legacy `RecordEnvelope` remain readable compatibility
  formats. They retain adapter identity, computation-head chaining, and legacy
  evidence behavior only for existing bundles.
- `ato.replay@2` uses `RecordEnvelopeV2` and operation routing. New v2 capture
  must not generate computation heads from Record data.
- Legacy records are not mechanically reinterpreted as v2 operations when the
  old event is not applicable. Migration requires an explicit protocol
  mapping; legacy output/evidence is moved to run logs or discarded according
  to retention policy.

The asynchronous writer, segment storage, frontier sealing, and capture
barrier are specified separately because they replace the legacy one-file,
fsync-per-observation write path.

## Supersession boundary

Once accepted, this RFC supersedes the observation-as-Record and
Record-commit-advances-computation portions of the accepted Protocol Adapter
and Computation Architecture RFCs for v2 only. The v1 compatibility reader
remains governed by the older contract.

## Non-goals

- deterministic replay;
- domain-specific state simulation in Player;
- treating output or presentation evidence as Records;
- deriving a ComputationRef from Record payload, order, frontier, or recording
  implementation metadata;
- redesigning canonical Computation residual identity in this RFC.
