# ADR-032: Runtime Network source objects (3c-b)

Status: proposed; implementation under review. This does not change K, D,
resolver v1/v2, receipt authority, Hosted upload or saved-artifact replay (3d).

## Identities and admission

`content_ref` and `archive_digest` are the SHA-256 of archive bytes. The
`closure_ref` remains the existing resolver's tree identity, including its
resolver contract and selected subdirectory. Repacking or compression changes
transport identity without changing tree identity, K or D.

A requester freezes to an anonymous file in its work root, hashes it in 64 KiB
chunks, verifies/measures the same open file, and plans from a temporary tree.
It reserves an upload, streams the file, finalizes it, then submits a small
SatisfyRequest containing content_ref, archive_digest, archive_bytes and
closure_ref. The old 32 MiB inline wire remains accepted, but is normalized to
the same owner-scoped, verified source object before search admission.

## Storage and completion

Reuse ato-api's STORE_BUCKET and D1. Migration 0298 adds only Runtime Network
source reservations; it does not alter 0296 or 0297. Owner + digest is unique;
the bucket key includes the reservation id. Identical bytes under another
owner do not grant that owner access, nor expose cross-owner deduplication.

Pending -> uploading (SQL compare-and-set) -> uploaded -> ready. A ready row
is immutable, and source acquisition only accepts ready. R2 writes use
If-None-Match:* and SHA-256. A FixedLengthStream bounds actual bytes regardless
of Content-Length. Finalize independently re-reads stored bytes and computes
size + SHA-256 before its SQL promotion. It never trusts metadata or ETag as
content evidence. Competing finalizers can promote only that immutable object;
a repeated upload cannot write an uploaded/ready object.

The official [R2 Workers API](https://developers.cloudflare.com/r2/api/workers/workers-api-reference/)
defines conditional writes and checksum verification;
[FixedLengthStream](https://developers.cloudflare.com/workers/runtime-apis/streams/transformstream/#fixedlengthstream)
errors on too many or too few bytes. These guarantees supplement, rather than
replace, finalization's read/hash check.

Limits are independent: 256 MiB per archive, four pending objects / 512 MiB
per owner, 30-minute admission expiry, and 16 objects / 2 GiB retained per
owner. Expired reservations deliberately continue to occupy quota: this
bounded first version has no automatic garbage collector and cannot create
unlimited abandoned keys. Exhausted quota requires later lifecycle work; it
is not silently refunded. A worker crash in uploading fails closed; the
reservation is not recycled over potentially in-flight storage writes.
Requester upload/storage quota is not Runtime logical transfer budget.
Cloudflare's [HTTP request body limit](https://developers.cloudflare.com/workers/platform/limits/)
can be stricter than the 256 MiB object cap (100 MB on Free/Pro). This version
uses one streamed PUT, not multipart upload; 128 MiB local acceptance is not a
claim that a 100 MB ingress admits it. Deployment must retain the provider's
lower limit. The >32 MiB acceptance includes a 64 MiB case below that limit.

## Acquisition authority and budget

There is no public digest endpoint and no signed download URL. The existing
attempt source endpoint authenticates the Runtime, checks assignment, claim,
search/request state, expiration and the ticket fence on every acquisition.
Object-reference requests require x-ato-attempt-fence. Legacy inline requests
retain their old client's header compatibility; supplied fences are always
checked. UNKNOWN, expired/unclaimed and foreign Runtime attempts cannot fetch.
The ready object's owner grant is checked again. An already-admitted streaming
response is bounded by the client's 15-minute timeout, as with the old relay;
no long-lived URL bypass is added.

The existing 0297 ledger reserves transfer from verified_bytes, consumes on
claim, and charges once per attempt. Redelivery within that attempt is free;
another attempt costs again. UNKNOWN never refunds or mints a new search id.
The Runtime acquires its durable journal reservation before downloading; a
source failure cannot erase Started, Finished, UNKNOWN or unreadable history.

## Bounded execution path

The Network Runtime streams into an anonymous file with a 64 KiB buffer and
an actual byte cap. It verifies bytes before measurement or extraction. The
file-backed resolver makes sequential file passes using the same tree and
symlink/path rules as the memory API; hashing individual files is also
streaming. Compressed streams have a decompression cap and zstd window cap;
raw tar metadata is checked before the library can allocate GNU/PAX payloads.
No Network path converts the archive back to Vec or read_to_end. Partial
files and trees are owned by temporary-file/directory handles and discarded.
One source connection per served attempt, one upload per submission, no
implicit retry, 15-minute transfer timeout and free-disk admission are
separate from logical budget. Existing artifact publication remains separate;
its size failure does not remove a K receipt.

## Compatibility and rollout

Merge ato-api and ato wire changes as a pair after review. Future deployment
order: migration 0298 -> API -> Runtime binaries -> requester binaries. Existing inline
clients can use the new API. New object-reference clients require that API;
old Runtime binaries should not receive archives above their 32 MiB limit.
No deployment, remote migration or flag change is part of this work.
Golden `satisfy-source-object.json` and its byte digest are shared by both repos.
