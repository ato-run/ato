# Verified raw archive to existing Search — 2026-10-24

Branch: feat/formation-approved-source-search, child of Rust #1526.

The shared intake and existing requester were exercised against a GitHub-shaped
raw gzip archive with an explicit wrapper directory entry and two regular files.
No application was executed and no archive re-encoding was used.

Measured facts:

- Compressed bytes: 232, format ato.source-archive.raw-tar-gzip/1.
- Archive SHA: sha256:ae9747eee7321058334239479e6b03e9eaec253e1e024b4800656de06b072f53.
- Full tree SHA: sha256:7d67edf215408291bec868c879d1bffe8347680bfe98bed0e6fdbc50d72c9a14.
- Root closure: sha256:7b373bd83a9efeee6f588bd5b2fdc939175aebed67815e4488ca9c3b9598f5a1.
- Verified entries: two; expanded regular-content bytes: 89.

The exact compressed bytes reached SourceTransport unchanged. Independent
FileVerifiedArchive verification and Runtime `freeze_verified_file` produced
the same tree/root closure and exposed app.py at the normalized root. The
wrapper directory was absent. Existing FrozenSearch canonical bytes and owner
K remained equal; ProposalRequestV2 included RepositoryEvidence. Corrupt archive,
tree, subdirectory, closure, attempt/transfer/expanded budgets and K were refused. An explicit zero-D
frontier also ignored a malformed, unselected source capsule.toml even under an
exploration policy; no known preset fallback was added.

The existing `proposal_search` example was run in its offline preregister mode
with the measured archive, exact typed request and deliberately nonexistent
mutable source directory/token file. It produced preregistration without
opening requester credentials, creating a Search or invoking a provider. API
ownership/provenance association and actual normal-job automatic handoff remain
separate pending acceptance, rather than a synthetic Formation success.

Full worker regression: 382 passed, one existing ignored; after the final
explicit-frontier regression, 126 library tests and all-target Clippy were rerun.
Formatting/diff checks also pass. The existing exact normal invocation is:

```
ato-formation-worker --api-base <configured-api> --job-id <pinned-owner-job> \
  --compute-id <exact-job-compute> --capsule-revision-id <exact-revision> \
  --work-root <owned-.tmp-root> --worker-id <authorized-worker>
```

No generic claim, shared daemon restart, paid provider call, API/Runner install,
feature flag, migration or deployment was performed.
