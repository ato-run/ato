# Bounded Adaptive Formation — finite escalation

Status: implementation / acceptance in progress (not completed).

This closes Formation Phase 4 through bounded decision/producer integration
within one search, not an agent loop or original Phase 5. Original Phase 5 is
roadmap 6c: external-trigger, authorized known-D-first background revalidation. DecisionProvider still cannot create D or decide K. CandidateProducer
cannot decide K. Only the existing Runtime/Verifier receipt establishes same-K PASS.

## Core contract

`ChoiceAction::EscalateToCandidateProducer {}` serializes only as
`{"kind":"escalate_to_candidate_producer"}`. Unknown fields are refused. Choice
identity uses the existing JCS `(seq, typed action)` digest. The provider submits
only an offered `choice_id`, never operations, source IDs, paths or policy.

Only a deterministic `default_next() == OpenProposalRound` authorizes offering
Escalate. The decision layer does not reimplement UNKNOWN/effect/owner/budget/
deadline/source-cost authorization. At that frontier, choices are Escalate
(default), unrecorded read-only inspections, Stop. Without decision policy, the
original OpenProposalRound behavior is unchanged. Provider error/timeout/invalid/
out-of-set uses the existing fallback to that deterministic proposal action.

A settled escalation releases the existing proposal action while no durable
proposal round exists. Once a round exists, the decision is consumed, including
fallback decisions; generated candidates therefore get the next finite decision
point. Restart reuses the settled choice and existing claim/completion fences.
There is one proposal round, no second generation, new executor or state machine.

## Receiver and gates

The receiver adds the union member and pins Rust authority WASM. `choices_json`
remains an opaque durable record; decision submission and existing migrations do
not change. Receiver compatibility is reviewed/merged before requester/core.

Completion requires A0–A9 with fixed providers and actual local Coordinator/D1,
Runtime and Verifier. Unit scheduler/compiler tests are not actual acceptance.
No live model calls, deploy, remote migration, or #1421 changes are authorized by
this implementation. Progress and exact pins belong in the ops ledger.
