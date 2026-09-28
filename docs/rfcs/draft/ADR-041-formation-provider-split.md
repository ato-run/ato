# ADR-041 — DecisionProvider is not CandidateProducer

Status: proposed architecture correction, documentation-only PR A, 2026-09-28.
No behavior change, migration, model call, merge or deployment in this PR.

## Context and detected mismatch

The original Formation/Continuation design separates known-D selection, new-D
proposals, Ato authorization/execution and Verifier evidence. The current
ADR-037–040 track uses Jev's finite entrypoint selection plus an Ato compiler.
The roadmap incorrectly made its efficacy progression the Candidate Generation
critical path. A new canonical digest after finite selection is not evidence
that a general LLM CandidateProducer has been implemented or is effective.

The original hosted note required sign-in and was not readable in this work.
[Formation](Formation.md) records the explicit user-supplied correction, including
this source limitation; it does not purport to replace the original note or the
Continuation Model. No unseen document is cited as independently verified.

## Decision

`DecisionProvider(SearchState, AllowedChoices) -> Decision` and
`CandidateProducer(SearchState, OperationCatalog) -> Proposal` are separate
provider-neutral interfaces. Jev implements the first. A general LLM will
implement the second, after a fixed producer proves the full authority path.
Ato, not either provider, validates, authorizes and canonically compiles D.
Only Verifier establishes K; a provider's choice/proposal is not a receipt.
The precise proposal/policy/persistence and P0–P11 contract is in
[Formation](Formation.md).

Reclassify 5a as completed Decision Layer, 5b as Candidate Generation next,
5c as pending Adaptive Formation Loop. Split 6 into independent 6a coverage
measurement, 6b capability expansion and 6c continuous adaptation. Known-D
20-app measurement may start without 5b or E2 efficacy.

## Preserve history, do not reinterpret wire/storage

ADR-037–040 remain unchanged historical records. Preserve GenerationDraft,
same-K compiler, durable claims and concurrency/restart fences, context/1–2,
point/1–3, prompt/1–3, failure projections, 0303 and E1/E2 fixtures/ledgers.
Reuse them as bounded-selection / proposal-validation infrastructure; do not
label their historical results canonical LLM CandidateProducer acceptance.
Do not change existing behavior just to align terminology in this docs PR.

#1420 is merged (`34c2ef2a8882f6fc53438c1e0f1722b240ed3d6e`). #1421 is open
at observed head `dc0fba4e0ecdd585985320551b52b72768002a5c`. It remains an
unexecuted, nonblocking finite-choice evaluation asset. No merge, oracle,
model call, 72-cell run, history rewrite or further 5b-gate hardening.
E1/E2 results do not measure general LLM CandidateProducer efficacy.

## Delivery and consequences

- PR A: this ADR, roadmap and repository Formation boundary companion only.
- PR B: proposal schema/catalog, validator/compiler, registry integration and
  fixed producer; preserve no-policy behavior and immutable frozen candidates.
- PR C (ato-api): new durable proposal receiver and additive migration after
  checking current migration numbering; never edit 0303. Receiver must merge
  before requester integration, with explicit approval for each merge.
- PR D: separate general LLM adapter/source policy and live acceptance, after
  fixed-producer P0–P11. No provider product/model name in semantic interfaces.

Do not claim implementation from this ADR. Record implemented, locally/integration
verified, merged and deployed as separate states. Actual Runtime/Verifier
acceptance must not be inferred from fixtures, compiler tests or mock receipts.
