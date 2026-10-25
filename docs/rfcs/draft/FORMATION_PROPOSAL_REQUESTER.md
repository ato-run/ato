# Fixed CandidateProducer requester integration (C2)

Status: implementation under review; no remote rollout or general LLM adapter.
Depends on ADR-041, ato #1423, and ato-api #700 merge
`b81ef143caac3f8e478b0954e9a5db2932c99f19`. C1 remains pinned throughout local
acceptance; migration 0304 is initialized only in an isolated local D1.

## Explicit authorization and independent I

`Submission::enable_candidate_producer(ProposalAuthorization)` is an explicit
owner opt-in before submission. Merely setting a public policy field does not
initialize the producer path. `prepare_proposal_submission` takes explicit K
and authorization and freezes source **without planning any known D**, reading
any base recipe, or invoking a Preset. Existing `prepare_submission` remains
the known-route path and requires explicit subsequent enablement.

Before enabling, the requester re-verifies the captured archive digest and
closure, materializes a private inventory, and authorizes only its regular
files. Source-directory edits after capture cannot authorize another file.
Symlink files/directories and paths outside the root cannot authorize an
entrypoint. Python module IDs must resolve to an inventory `.py` or a package
with `__init__.py` and `__main__.py`; intermediate packages require their own
`__init__.py`. Host-installed packages and implicit namespace packages do not
satisfy this v0 authorization. K, policy and Runtime constraint stay frozen.

There is no new semantic primitive: `initial_source` is a SearchState input DTO.
The private original recipes and frozen authorization are retained separately
from admitted generated recipes. Generated D cannot become a modification base
by appearing in a later receiver response.

## Request projection and call grant

`ato.formation-proposal-request/1` contains search ID, frozen K, Runtime
constraint, known D refs, bounded enum failure/inspection summaries, opaque-ID
OperationCatalog and remaining attempt/proposal allowance. It contains no
source text/bytes, private paths or source-domain map, Binding values, raw
receipts/logs, credentials or arbitrary Runtime/host facts. Even the fixed
fixture producer uses this same production request construction.

`serve_proposal` observes an open point and validates the receiver's frozen
search against the requester's own frozen search. It marks the local claim
attempt before POST, receives a successful revision/round/private UUID fence,
then invokes the producer. A lost/error/409 claim response never invokes it.
A subsequent GET showing claimed does not grant a fence. Concurrent requesters
must win the same receiver CAS, not an in-process mutex masquerading as a lock.

The guarantee is **at most one invocation per durable grant**, not an assertion
that every granted round invokes once despite crashes. A restarted requester
cannot recover the fence. A thread waits only until the lesser of round expiry
and owner-authorized timeout; late results cannot be submitted. The fixed
producer has no external transport. Any future adapter must also bound/cancel
its own transport; this does not authorize a generic agent loop.

## Completion and loss recovery

Success sends exact `ProducerOutput.raw` bytes as canonical base64 with fixed
provenance. The requester does not parse them into a trusted ProposalBatch or
reserialize the proposal. Provider error/timeout closes only the proposal round,
never creates a K failure or attempt evidence. C1's fixed-only provenance
contract is unchanged, including absent provenance for provider errors/timeouts.
General provider/model/usage/cost/error provenance belongs to a versioned PR D.

Completion JSON is serialized once and POSTed once. This v0 implementation
**does not retry completion**: on any uncertain response, the caller resumes
GET. A durable completion is reused; if the write did not land, the spent claim
expires. Neither case generates a new proposal. This deliberately avoids a
second output or non-identical payload under an old fence.

## Independent recompilation before local admission

On every terminal-round observation, the requester checks raw length/digest,
fixed provenance and immutable frozen search, invokes its native Rust
CandidateRegistry/ProposalValidator on the raw batch, and compares every member
in order with the receiver: outcome, proposal ID, D ref, exact recipe, bound
Derivation and SearchCandidate. The generated candidate vector in SearchState
must also equal the recompiled vector. Once accepted, the exact terminal-round
value is pinned locally. Missing/changed results fail closed before admission.

Only after all comparisons succeed are the canonical recipe map and receipt
Contract map expanded, with each D mapped to the original frozen K. The frozen
known candidate vector is unchanged. No compiler or DecisionProvider answer is
PASS. Existing placement/ticket/source verification, Runtime execution, common
observation/Verifier and requester route acceptance establish the result.

## Acceptance scope

The fixed-only `proposal_search` and thin `proposal_runtime` examples use
production requester/DecisionProvider/Runtime methods. The local harness uses
production C1 routes and ordinary runner-token authentication, a fresh D1/R2,
and a filesystem-only SQL inspection/fault channel. Loss tests forward real
requests then drop responses; concurrency holds both real claim requests before
forwarding. UNKNOWN uses the real Runtime with an unwritable attempt journal.
None of these mechanisms supplies a PASS result or edits K.

The zero-D fixture vendors unmodified CPython 3.12.7 HTTP server source at
`0b05ead877f909b7efe712db758012d9dbece7ce` with its license and a preregistered
logical-port launcher. There is no preset/capsule.toml/base D. This is a small
OSS-based HTTP fixture, **not** evidence of unassisted general real-world LLM
coverage. The bad/good batch is fixed before execution, not chosen after results.

Actual results and exact binaries/receipts are recorded in the
[C2 ops ledger](../../ops/formation-proposal-requester-2026-09-28.md);
unit tests are not that actual acceptance. Original Formation Phase 3/4/5
mapping to 5a/5b/5c is unchanged. Escalate remains absent until 5c. No source
transmission, model calls, source patches, shell/argv generation, >1 round,
remote migration or deployment is part of this PR.
