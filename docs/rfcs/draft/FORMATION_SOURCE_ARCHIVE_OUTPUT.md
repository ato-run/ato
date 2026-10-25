# Source-only Formation physical output

Status: Draft. This is an additive operational request/output contract, not a
Capsule, Contract, Derivation or Computation identity profile. Implementation is
split across this Rust worker and the matching API receiver. It is not deployed
or advertised by the existing application worker.

## Request and authority

`ato.formation-job.v1` may exclusively request `source_archive` with policy
`formation-source-archive-v1`, a pinned public GitHub source, `network = denied`,
`publish_enabled = false` and no authoring. Network denial applies to application
effects; this profile explicitly permits bounded pinned-source transport and
attempt-authorized object transfer. No application is executed. Existing omitted
fields and application job digests remain unchanged.

The existing Runner principal, Formation job, attempt and attempt fence authorize
all control operations. The dedicated `source-archives/claim` path reuses the API
claim service with an exclusive output filter. Optional `--job-id` adds an exact
`job_id` claim query; the worker rejects an unrelated returned assignment. Without
that flag, the dedicated queue behavior is preserved. The normal claim path must exclude
this output by default; the normal Rust job executor also refuses it before
journal reservation, source fetch, manifest parsing or build. There is no fallback
from an absent dedicated route, and no legacy snapshot-builder credential.

The canonical JCS binding contains `job_id`, `attempt_id`, `attempt_fence`,
`job_digest` and `archive_descriptor`. The job digest binds the exact repository,
commit, selection and output profile. The descriptor contains:

- `archive_format_version`, `source_archive_digest`, `size_bytes`;
- `source_tree_digest`, `resolver_contract_version`, `subdirectory`,
  `source_closure_ref`;
- `observed_file_count`, `observed_uncompressed_bytes`.

Metrics describe the verified full archive tree. Files include empty regular
files and contained symlinks; directories and tar metadata are excluded. Expanded
bytes count regular content only. Subdirectory selection is checked by the same
verifier but does not redefine the full-tree digest or counters. Only actual
`ato.source-resolver.v1` / `.v2` are admitted. No historical projection alias is
introduced.

## Transport and terminal output

The verified original bytes are streamed without an archive-sized allocation.
Their transport labels are `ato.source-archive.raw-tar/1`,
`ato.source-archive.raw-tar-gzip/1` or `ato.source-archive.raw-tar-zstd/1`. These
labels do not claim the historical canonical `ato.source-archive/v1` encoding.
The original SHA-256 and all verifier measurements remain unchanged.

Attempt-scoped upload authorization returns an upload ID, the exact binding
digest, transient upload/download URLs and an epoch-millisecond expiry. URLs are
not logged, persisted or passed to the application. Object PUT/GET requests never
receive the Runner bearer token. PUT explicitly sets the exact descriptor
`Content-Length` and uses a finite sized reader, matching the API's signed length
constraint; it does not use chunked transfer. The strict client disables redirects, ambient
proxies and transparent decoding. The original intake retains the existing
256 MiB transport ceiling, 50,000 files / 512 MiB expansion ceiling and frozen
15-minute deadline. Control responses are capped at 64 KiB; readback is capped at
the exact expected archive size, and is reverified for SHA, size, format, tree,
resolver, closure and counters before completion.

The terminal response is the closed physical output
`kind = source_archive`, `status = source_materialized`, the binding/digest and
owned source revision/materialization IDs. It has no FormationResult,
ComputeSchemaRef, ContractRef, seal, `fully_satisfied` or application success.
Owner archive admission consumes this exact immutable attempt output. A separate
exact-archive approval is required before application planning/build.

## Cleanup and deployment dependencies

All local files live in the explicitly owned work-root `.tmp`. Selection scratch
is removed before transfer; archive files are anonymous handles. Failure or a bad
completion acknowledgment triggers authenticated deletion of the exact owned
staging upload, with a separate bounded ten-second cleanup allowance. Shared
content-addressed final objects are never deleted. Unconfirmed cleanup remains
failure, never successful materialization.

The API must scope every ticket and CAS output transition to the current owner,
job/attempt/fence/binding. It must keep cancellation, immutable acceptance,
idempotent completion and owned-staging cleanup correct across lost responses.
Malformed authorization responses that cannot identify a safe upload ID are
rejected; server-side reserved-ticket expiry/recovery must reclaim any orphaned
owned staging. The worker does not claim crash recovery or server-side fencing
from its HTTP fixture tests.

Receiver-first rollout is mandatory. A worker with this command cannot be
installed against an older receiver. Raw encodings are not deterministic across
provider archive rewrites: an existing conflicting revision/format digest must
remain a conflict, not overwrite or upgrade immutable evidence. This slice does
not change that storage invariant, add a daemon/table, enqueue an application,
change a feature flag, or deploy a worker.

## Explicit existing Requester client handoff

The existing `proposal_search` example accepts optional
`formation_source_pointer` and `expected_source_origin` members in its existing
configuration. They are an operational extension, not another configuration
schema, CLI, semantic profile or authority. Both members require the existing
`source_job` / `source_archive` / owner-authored `frozen_request` lane. The local
archive verifier envelope is not a new server Job and does not replace the
accepted source-only Job named by the pointer. In pointer mode the original
`frozen_request` JSON value must equal its typed serialization; normalization
such as null becoming omission is rejected before credentials or network use.

`FormationSourceOrigin::validate_local_source` reuses `SourceArchiveOutput`, its
canonical physical Binding and the original source verifier. It checks the
pointer, fixed request/source tuple, actual raw archive digest, tree, format,
resolver, closure and counters without changing K, D, policy or budget. A saved
origin is consistency evidence; it is not ownership, approval, current attempt
or execution authorization. Those remain the existing API authority.

After that preflight, `Client::submit_from_formation_source` calls only:

1. `POST /v1/runtime-network/sources/from-formation` with the exact pointer.
2. After the full transferred source/origin matches the saved snapshot,
   `POST /v1/runtime-network/satisfy` with the unchanged typed request and the
   separate `source_archive_job_id` / `binding_digest` query parameters.

There is no `/sources` requester upload, archive re-encoding, fallback or
automatic POST retry in this path. Metadata responses are capped at 64 KiB,
redirects and ambient proxies are disabled, and the existing bearer/deadline
mechanism applies. The example's finite observation allowance also bounds this
handoff; an explicit repeat reuses the same Search/body and asks the server to
replay its existing result. It does not reset durable Search/provider budgets.
The existing requester claim, provider, receipt acceptance and observation loop
are reused unchanged. An accepted request or stopped observation is not K
satisfaction. Missing pointer/origin members keep the existing legacy upload
and plain `/satisfy` behavior.
