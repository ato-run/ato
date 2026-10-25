# Explicit verified archive intake for the existing Search requester

Status: draft. Rust preparation and explicit requester caller implemented;
normal Formation automatic Search handoff is not enabled.

Normal claimed jobs keep their existing authored/preset planning and executor.
Their source verification now shares the same bounded file-backed intake as an
explicit Search requester: pinned compressed SHA, actual size, measured full
tree, resolver and selected closure. Source-only output or policy cannot enter
normal execution before source fetch or attempt-ledger reservation. Existing
one-shot `--job-id` already provides exact normal job claims; no new scheduler,
claim endpoint or CLI selection is added.

The existing proposal requester example accepts explicit `source_job`,
`source_archive` and `frozen_request`. The latter is the existing typed
SatisfyRequest, including the owner-authored BoundContract, policy, proposal
authorization, finite Search budget and exact source. The old contract/policy
configuration must agree with it; a local file or manifest never supplies a
fallback K. The existing requester auth and `/satisfy` service remain the only
authority for creating Search and spending its durable budget.

`prepare_source_proposal` requires immutable uploaded-archive source, explicit
expected full tree, empty subdirectory and denied network. It consumes the
normal intake's FileVerifiedArchive. `prepare_pinned_proposal_submission` copies
its original raw bytes with finite counters/deadline, freezes the verified root
using the existing Runtime path, and enables the existing CandidateProducer.
It does not re-tar, rename resolver identities or infer known Ds. Prepared
request canonical bytes must exactly equal the supplied request. Initial scope
has an explicit zero-D frontier; nonempty known D sets and subdirectories are
reported unsupported. Repository inference is separate ProposalRequestV2
repository_evidence and cannot change K, the permission ceiling or budget.

API ownership binding remains a separate required slice. An opaque accepted
source job/binding digest may be an explicit operational envelope on the
existing owner-authenticated `/satisfy` request. It must check the same owner,
accepted current attempt/fence, raw SHA/tree/closure and immutable provenance,
with actual bounded source-store transfer/rehash. Source archive approval is
not K/Search authorization. An ordinary Formation claim has no such association
today and must not create Search, reuse the owner's credentials, emit a normal
Formation success or consume model calls implicitly.

No new table, K DTO, Capsule semantic root, implicit D selection, application
execution, API wire field, production/staging deployment or provider call is
part of this Rust slice. Replay/UNKNOWN protection and retry/failure evidence
continue to use the existing Search/Proposal durable claims and finite budget;
preparation does not itself claim an execution or produce a verification seal.
