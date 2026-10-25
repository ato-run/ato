# Bounded Model Object hydration

Status: draft. Owner: Connected Realization Worker.

Wan staging's 136 immutable software objects took about five minutes to hydrate
serially, before its 18.4 GB and 11.4 GB model weights. Object parallelism alone
still exceeded the unchanged hosted Contract attempt deadline on Secure A40:
the 11.4 GB object completed in 399 seconds while the 18.4 GB object remained
incomplete at ten minutes. Extending that deadline or changing the declared
inputs is outside this change.

Hydrate at most four distinct object digests concurrently across the already
granted Model Sets, scheduling larger objects first. Fetch and validate every
manifest before hydration; reject conflicting sizes for a repeated digest
before object I/O. One writer owns a digest within this delivery. Repeated
paths reuse the verified marker and record a zero-transfer hit.

Each object uses batches of at most four 256 MiB ranges, so at most sixteen
range requests run across the four object workers. Each response streams into
a private temporary file, with a strict length bound and five attempts. Join
the entire batch before appending completed ranges in order to the resumable
contiguous prefix. Failed batches leave the existing prefix intact; temporary
files are removed on exit and stale files are discarded before resuming.
Temporary range storage is bounded to four chunks per object (plus one byte
per response for detecting excess bytes). No sparse file length is treated as
evidence of a contiguous resume offset.

Reuse full SHA-256 verification and the Runner-only cache seal after the
prefix is complete. Stream bytes to disk; do not buffer whole objects or
pass delivery credentials to the workload.
Poll the existing control-plane stop fence and refresh mutable execution
authorization under a mutex before each chunk, including Runs without a renewal
monitor. A failed object or refresh prevents new work, and all workers are joined before
any input is materialized or the workload starts. In-flight chunks remain
bounded by the existing request size/timeout and stop at their next refresh.

Delivery reports retain declaration order and actual requested/transferred
bytes, verification timing and per-object cache status. Scheduling and elapsed
time are physical realization evidence, independent of ContractRef and D.
No schema, identity, grant expiry, enrollment, price bound or provider special
case is added.

Validation: exercise overlapping bounded workers without timing thresholds,
deterministic reports and warm zero-transfer reuse, digest deduplication and
size conflicts, authorization revocation before fetch, overlapping range
requests and body reads, ordered prefix assembly, batch interruption and
resume, stale-file cleanup, actual retry byte counts, and the existing
corruption/short-response/marker tests. Real cold/warm/fresh GPU delivery
and Wan generation/save acceptance remain necessary after deployment.
