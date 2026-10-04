# Bounded Model Object hydration

Status: draft. Owner: Connected Realization Worker.

Wan staging's 136 immutable software objects took about five minutes to hydrate
serially, before its 18.4 GB and 11.4 GB model weights. The unchanged hosted
Contract attempt deadline expired before the workload could start. Extending
that deadline or changing the declared inputs is outside this change.

Hydrate at most four distinct object digests concurrently across the already
granted Model Sets, scheduling larger objects first. Fetch and validate every
manifest before hydration; reject conflicting sizes for a repeated digest
before object I/O. One writer owns a digest within this delivery. Repeated
paths reuse the verified marker and record a zero-transfer hit.

Reuse the existing bounded range, retry, contiguous partial-file resume,
full SHA-256 verification and Runner-only cache seal. Stream bytes to disk;
do not buffer whole objects or pass delivery credentials to the workload.
Poll the existing control-plane stop fence and refresh mutable execution
authorization under a mutex before each chunk, including Runs without a renewal
monitor. A
failed object or refresh prevents new work, and all workers are joined before
any input is materialized or the workload starts. In-flight chunks remain
bounded by the existing request size/timeout and stop at their next refresh.

Delivery reports retain declaration order and actual requested/transferred
bytes, verification timing and per-object cache status. Scheduling and elapsed
time are physical realization evidence, independent of ContractRef and D.
No schema, identity, grant expiry, enrollment, price bound or provider special
case is added.

Validation: exercise overlapping bounded workers without timing thresholds,
deterministic reports and warm zero-transfer reuse, digest deduplication and
size conflicts, authorization revocation before fetch, and the existing
corruption/short-response/resume/marker tests. Real cold/warm/fresh GPU delivery
and Wan generation/save acceptance remain necessary after deployment.
