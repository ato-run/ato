# Capsule Object Transport Client

Status: Draft  
Owner: ato CLI / Objects  
Last updated: 2026-08-23

## Purpose

The object transport client sends a reachable content-addressed Capsule graph
to ato.run without first producing a base64 `.capsule` envelope. The portable
small-bundle format remains supported by `ato encap`; `ato upload` is the large
object lane.

Identity remains layered:

- the already-known `ComputationRef` is the logical root;
- the canonical graph index BLAKE3 is transport identity;
- each object BLAKE3 is physical content identity;
- each materialization descriptor is an independently addressed physical
  realization;
- a RecordFrontier referenced by a descriptor is capture provenance, never a
  computation-identity source.

The client MUST NOT derive a ComputationRef from index ordering,
materialization bytes, VM chunks, or RecordFrontier identity.

## Closure traversal

`ato-objects` owns protocol-neutral closure traversal. Computation extensions
provide `ComputationReferences`; Materializers provide
`MaterializationReferences`. The transport code does not parse replay,
workspace, RecordFrontier, or VM descriptor semantics.

The root graph edge includes physical materialization descriptors as transport
associations. Computation nodes include their canonical residual plus
extension-derived references. Materialization nodes include the references
reported by their registered Materializer. Other content is an opaque payload
leaf.

The exported descriptors are deterministically ordered by content ref and
contain sorted, unique adjacency. Limits match the server: 10,000 objects,
64 MiB per object, and 16 GiB logical closure. Every object is read through the
verified `ObjectResolver`; a size or digest mismatch aborts before PUT.

## CLI

```text
ato upload <selector> \
  --api-url https://staging.api.ato.run \
  --materialize ato.materialize.vm.snapshot@1 \
  --visibility private \
  --receipt object-upload-receipt.json
```

Authentication is supplied through `ATO_API_TOKEN` or `--auth-token`.
`ATO_API_URL` may supply the base URL. There is deliberately no production
default. Non-local HTTP, URLs with paths/queries/fragments, and empty tokens
fail closed.

`ato upload` reuses the same Materializer encode/verify boundary as `encap` but
does not create a `.capsule` byte envelope. A Materializer that cannot capture
the selected active Realization returns its own error; the uploader never
falls back to another Materializer.

## Prepare and authorization validation

The client computes RFC 8785-style JCS bytes for the graph index and submits
its BLAKE3 with an idempotency key. The default key is deterministic from that
index; a caller may provide a migration-specific key.

Prepare must return exactly one upload instruction for every declared object
while the graph is uploading. The client rejects duplicate, omitted,
undeclared, or size-mismatched instructions before sending bytes. It never
asks for, nor depends on, a missing-object list. An idempotent ready response
must contain no PUT instructions and must name the ready Bundle.

The server may request only `content-type` and `x-amz-meta-*` PUT headers.
Other response-controlled headers are rejected. Bearer authentication is sent
to same-API fallback PUTs, never to direct presigned R2 URLs.

## Bounded upload and retry

Object PUTs use a bounded worker count (1–32, default 4). Each worker holds at
most one verified object plus the request body. There is no unbounded task or
byte queue.

Network failures, HTTP 408, 429, and 5xx are retryable. Other 4xx responses are
terminal. Prepare and finalize use the same bounded retry policy and stable
idempotency key. PUT retries send the same digest-addressed bytes.

The client counts successful PUT objects/bytes. Finalize supplies the
server-measured same-tenant `objects_stored_new` and `unique_stored_bytes`;
the client does not infer those values from private CAS existence.

## Finalize, validation, and receipt

After every declared PUT succeeds, the client calls finalize and polls the
authenticated graph status until `ready`, `rejected`, or the configured poll
limit. `ready` without Bundle identity or server CAS accounting is rejected.

The canonical local receipt records:

- graph id;
- root ComputationRef;
- bundle index digest;
- ready Bundle id;
- object count and logical bytes;
- successful PUT count/bytes;
- server-measured new CAS count/bytes;
- every object digest;
- validation status.

Tokens, upload URLs, signed headers, and object bytes are never written to the
receipt.

## Compatibility and migration boundary

`ato encap` and `ato run <file.capsule>` retain the legacy/small local transport.
The new client consumes the additive server API from ato-api PR C1. Legacy
importer restacking is a later PR: it may call this client boundary, but it may
not convert an old Snapshot id or old VM byte digest into a current
ComputationRef.
