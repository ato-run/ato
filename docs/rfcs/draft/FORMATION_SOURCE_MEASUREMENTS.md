# Measured source evidence from the current Formation intake

Status: Draft. Receiver-first deployment is required; no deployment is included.

## Detected mismatch

The API's SourceMaterializationReceiptV1 comment points to the former
`crates/snapshot/src/source_receipt.rs`. That file moved to the old snapshot
provider and was deleted with the application execution monolith in
`4a5df3bc7` (2026-08-14). The current snapshot-builder registers opaque snapshot
materializations and does not execute that historical source-materialization
producer. This change neither resurrects that provider nor claims a deployed
source-only producer exists.

The current Formation worker already verifies acquired archives with
`FileVerifiedArchive` before source selection/planning. The existing
`ato.formation-result.v2.source_evidence` path is used for these actual observations;
no new receipt schema, storage table, semantic root, sidecar, fetch or build is
introduced to measure them.

## Existing wire field, additive measurement pair

The result contains its existing job/attempt/fence and `source_closure_ref`.
`source_evidence` carries:

- `archive_digest`: SHA-256 of the exact acquired compressed transport bytes,
  rechecked by the existing file verifier (and the declared expected archive digest
  when present).
- `tree_digest`: the existing measured full source tree digest.
- `resolver_contract_version`: the actual `ato.source-resolver.v1` or `.v2`.
- `subdirectory`: the exact selected relative application path, separate from
  the full-tree digest.
- `observed_file_count` and `observed_uncompressed_bytes`: an optional paired
  extension for historical receivers/results; the new producer emits both.

Count is the verifier's existing `max_files` counter: regular files, including
empty files, plus contained relative symlinks. Directories, tar metadata and the
stripped common archive wrapper do not count. Bytes are regular-file content
bytes, excluding headers/padding, directory records and symlink target text.
These facts are retained from the same successful bounded verification, never
inferred from headers or recounted after a build can change the materialized tree.
Both values describe the full archive, even when the application selects a
subdirectory. Default source bounds remain 50,000 entries/512 MiB file content;
the transport's existing 256 MiB bound is separate. JSON counters must be safe
integers. The counters do not enter tree/closure/K/D identity.

The worker captures evidence after actual subdirectory materialization succeeds
and before application execution. It rejects a job/request assignment mismatch
before fetching. The published result and its existing verification execution
identity bind the observation to the claimed job/attempt/fence; the API derives
the repository/pinned commit from that job's immutable source declaration.
Source success cannot publish a failed application Contract.

## Resolver and storage boundaries

The historical `ato.capsule-program-source-projection/v1` defines another tree
projection. Current measurements must not be relabelled with that spelling.
The old API validator currently admits only the historical literal; deploy the
receiver extension for the actual current resolvers and paired counters before
deploying this worker. No downgrade or invented capability model is added.
Old omitted `source_evidence` results/golden bytes and Contract receipts remain
unchanged.

GitHub acquisition currently yields raw codeload tar.gz bytes. The current
Formation worker does not upload those bytes to `source-archives/`. A result is
therefore measured source evidence, not proof of an owned persisted archive or
an `ato.source-archive/v1` materialization. API identity-only/materialization-null
is correct unless real stored bytes are separately confirmed.

Remaining execution work is a source-only path through existing authorized
source-materialization queue/claim/upload/report handling, with truthful archive
format, stored-byte digest/retrieval checks and owner approval before application
build. No snapshot-job/selection-version is fabricated from a Formation attempt.
