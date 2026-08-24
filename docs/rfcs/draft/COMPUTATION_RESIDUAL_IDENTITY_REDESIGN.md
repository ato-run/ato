# Computation Canonical Residual Identity Redesign

Status: Draft boundary note; design intentionally unresolved

## Problem

The current accepted architecture includes adapter semantic frontiers in a
computation residual and advances a Run head as observation Records are
committed. The operation-based Record model makes recording asynchronous and
separates Record identity, RecordFrontier identity, and ComputationRef identity.
Reusing the legacy observation hash chain would preserve the coupling under a
new name.

## Invariant fixed now

Record v2 MUST NOT derive ComputationRef identity from Record ordering, Record
payloads, RecordFrontier, payload digests, adapter implementation identifiers,
or other recording metadata.

The following identities are distinct:

```text
ComputationRef
!= RecordIdV2
!= RecordFrontierRef
!= Materialization descriptor reference
!= VM snapshot byte/object digest
```

A Record Writer may assign Record IDs, segment digests, and RecordFrontier
identities. It must not seal or update a ComputationRef. Computation sealing is
an explicit higher-level operation that may first execute a Capture Barrier and
then associate one or more Materializations with an already established
logical computation point.

## Deferred design question

This change deliberately does not guess a replacement canonical residual for
ComputationRef. A follow-up RFC must define:

- which semantic state is sufficient to identify a sealed computation point;
- how that state is captured independently of recording implementation;
- the authority and validation rules for sealing;
- how existing computation objects migrate without equating a legacy Record
  chain or VM bytes with the new identity;
- how Contracts validate candidate Realizations without generating identity.

Until that RFC is accepted, a caller must supply a known target
`ComputationRef`. Legacy heads may be read for compatibility, but they are not
promoted as the canonical identity model for Record v2.

## Non-goals

- deriving identity from replay success or Contract success;
- deriving identity from a VM snapshot or filesystem digest;
- deriving identity from a RecordFrontier;
- converting a legacy Snapshot ID into a current Capsule identity.
