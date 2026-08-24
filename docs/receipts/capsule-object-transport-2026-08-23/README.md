# Capsule object transport client receipt — 2026-08-23

Status: implementation verified locally; no staging upload

## Repository state

- Repository: `ato-run/ato`
- Base branch: `feat/vm-snapshot-materializer-v1`
- Base SHA: `a16efc06468c0132a3e374593ee111ce6510aba3`
- Branch: `feat/capsule-object-transport-client-v1`
- Initial implementation commit:
  `d60bd41f4f81b0ce6c38d372fa9e706090f94ec2`
- VM/frontier-bound uploader receipt commit:
  `8bd6630f`
- Depends on generic VM Materializer Draft PR:
  `https://github.com/ato-run/ato/pull/1295`
- Depends on server transport Draft PR:
  `https://github.com/ato-run/ato-api/pull/519`
- Draft PR: `https://github.com/ato-run/ato/pull/1296`

## Implemented boundary

- Extension-driven reachable closure traversal without base64 Bundle bytes.
- Separate ComputationRef, graph-index digest, object digest, and
  materialization descriptor identities.
- Deterministic adjacency and strict closure/object limits.
- New explicit `ato upload` command with no production URL default.
- Exact prepare-response closure validation.
- Bounded concurrent PUT (1–32), retryable error classification, and stable
  idempotency key.
- Direct presigned PUT without bearer leakage; authenticated fallback PUT.
- Idempotent finalize and bounded validation polling.
- Canonical receipt with client PUT and server CAS accounting.
- VM uploads re-read the canonical descriptor from the local closure and bind
  its existing target ComputationRef, VM descriptor ref, and sealed
  RecordFrontier ref into the uploader receipt before publication can proceed.
- Existing `ato encap` / local `.capsule` compatibility retained.

## Local verification

```text
cargo fmt --all -- --check
  PASS

cargo check -p ato-cli --locked
  PASS

cargo clippy -p ato-cli -p ato-objects \
  --all-targets --all-features --locked -- -D warnings
  PASS

cargo test -p ato-cli object_transport --locked
  PASS — 3/3 focused uploader tests

cargo test -p ato-objects --locked
  PASS — 23/23
```

Focused coverage includes exact instruction-set validation, bounded
concurrency, retry, validation polling, upload accounting, and the invariant
that changing VM physical bytes changes the object graph but not the supplied
root ComputationRef. The VM receipt test also verifies that the descriptor's
target equals that root and derives `record_frontier_ref` without changing it.

## Upload accounting

- Staging graphs prepared: 0
- Staging objects uploaded: 0
- Production objects uploaded: 0
- Production rows inserted: 0
- Production rows updated: 0
- Production rows deleted: 0
- Production approval requested: false

No API was called by the tests; the transport orchestration used a fake
provider. No staging or production mutation was performed.

## Remaining acceptance boundary

The CLI client exists, but current macOS cannot produce or restore a
Firecracker capture. The staging Linux active-Realization capture bridge,
validator deployment, graph-aware runner download, legacy importer restack,
and 2048 three-restore acceptance remain in PR D. Source/Replay/OCI execution
is not counted as VM acceptance.
