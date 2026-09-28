# Formation — selection, proposals and verification

Status: architecture contract for the correction in ADR-041; not a claim that
CandidateProducer or durable proposal rounds are already implemented. This
repository companion records the user's 2026-09-28 requirements. The original
[Formation note](https://cinst-tfgnfdkubznb7xh2.stg-app.ato.run/#root/iuv9VgcYHeL3/FIVzrv4ni7q2/WwsYaQA5nRAT?ntxId=CVDlt5)
and Continuation Model remain design sources; this is not a replacement or an
edit of those documents. The note required sign-in during this correction, so
its full text and the named Continuation Model attachment have not been checked.

## Authority boundaries

- **DecisionProvider MUST NOT create a new D.**
- **AI-generated new D enters the search only through
  CandidateProducer → ProposalValidator.**
- **CandidateProducer MUST NOT decide K.**
- **DecisionProvider MUST NOT decide K.**
- **Only Verifier establishes C' ⊨ K.**

```text
known D + admitted generated D → effective CandidateRegistry
    → DecisionProvider(SearchState, AllowedChoices) → Decision
        Attempt → Runtime → Observation → Verifier → PASS/FAIL evidence
        Inspect / safe Probe → inspection evidence
        Escalate → CandidateProducer(SearchState, OperationCatalog) → Proposal
            → Ato ProposalValidator / canonical compiler → new D → registry
        Stop → bounded termination
```

Jev is the DecisionProvider implementation: known-candidate ranking, next
Attempt, Inspect, safe Probe when available, escalation and Stop. It returns
only an offered choice_id, never proposal payload, D, argv, K or a verdict.
The general LLM interprets authorized source/manifests and failure evidence to
propose new D, typed modifications or unsupported. Ato owns schema/catalog
validation, authorization, Binding/network/effect policy, canonicalization,
D identity, registry, admission, budget and fences. Runtime owns execution.
A model-judged Browser observation, if explicitly specified, belongs to a
Verifier implementation, not the DecisionProvider.

## Provider-neutral proposal contract

```text
CandidateProducer::propose(ProposalRequest) -> ProposalBatch
ProposalRequest {
  schema, search_id, frozen_contract, runtime_constraint, known_derivations,
  source_context, failure_evidence, inspection_evidence, operation_catalog,
  remaining_budget
}
ProposalBatch { schema: "ato.formation-proposal/1", proposals: Proposal[] }
Proposal {
  kind: propose_derivation | modify_derivation | unsupported,
  base_derivation_ref?, operations: OperationInvocation[]
}
```

K, Runtime constraint and policy are read-only, never provider outputs. Ato
computes proposal identity and DerivationRef from validated canonical bytes;
the provider must not supply them. Modification may reference only an
already-authorized base. Unsupported carries no executable operation.

Ato issues a per-search OperationCatalog of typed parameter domains, e.g.
`python_script@1 {entrypoint_id}`, `python_module@1 {module_id}` and
`static_http@1 {root_id, spa_fallback: boolean}`. These are examples of reviewed
vocabulary, not a claim all are implemented. IDs are opaque owner-authorized
logical references, resolved privately against immutable source. Providers
never generate paths, shell, arbitrary executable/URL, or full capsule.toml.
First slice composes operation and arguments into a D compiled *after* the
proposal; choosing a precomputed DerivationRef alone is not generation.

## ProposalValidator

For each proposal, validate strict schema → operation catalog → argument
domain → authorization/Binding/network/effects → canonical AuthoringDraft →
existing bind/compiler → generated K equals frozen K → canonical D → dedup →
CandidateRegistry. The compiler is not a second verifier. Admission and actual
Runtime/Verifier receipts remain mandatory, including requester rechecking.

Reject unknown fields, changed/weakened K or removed conditions, unauthorized
operations, shell/executable/path/URL injection, new secrets/Bindings,
network/effect expansion, changed Runtime constraints and duplicate D. A mixed
batch admits valid unique proposals independently; rejection is not K failure.
Frozen known candidates remain immutable. Both deterministic scheduling and
DecisionProvider use one effective iterator: known D plus admitted generated D.

## Policy and bounded execution

CandidateProducer context need not share Jev's compact finite-choice privacy
projection. Explicit policy may allow bounded README/manifest/relevant source
excerpts, K, Runtime facts, known D summaries and failure/inspection evidence.
External source transmission requires explicit opt-in. Never send credentials,
secret values, unrelated files or arbitrary host identity.

Policy names provider, allow_source_text, max_source_bytes, max_proposals,
timeout_ms and max_proposal_rounds. v0: **one round, one call, at most four
proposals, timeout ≤30 seconds**. Provider error/timeout yields deterministic
candidates_exhausted/unsupported, never fabricated attempt or K-failure evidence.
No unbounded agent loop, shell repair, source patching, automatic Runtime
migration or new execution permission.

## Durable receiver and registry

Migration 0303 and old generation rows keep their meanings. Add new proposal
round/candidate storage (0304 only if the number remains available at
implementation time). Records are append-only, one durable claim and completion
per round, one concurrent winner, revision CAS, write-once generated candidates.
No provider call before successful claim. Crash/lost response after claim does
not permit retry; restart reuses completed proposals. UNKNOWN, owner stop,
in-flight work and cumulative attempt/byte/deadline budgets take precedence.
Opening a round spends its allowance regardless of provider outcome.

Offer escalation only after producer integration exists and round budget
remains. Candidate completion does not authorize execution by itself. Decisions
and attempt issue recheck the same fences. Durable state is orchestration and
evidence, never Capsule identity.

## Acceptance

First use FixedCandidateProducer and fixed DecisionProvider (or Jev in a
separately authorized live run), not mandatory live LLM. Record actual Runtime,
Verifier receipt and requester acceptance separately from mocks.

| Case | Required evidence |
|---|---|
| P0 | No generation policy: known-D deterministic behavior unchanged |
| P1 | Actual known D1 FAIL, exhausted pool, one producer call, two proposals, D2/D3 admitted |
| P2 | DecisionProvider selects generated D3; actual Runtime/Verifier PASS and requester acceptance |
| P3 | Selected D2 actually fails K; durable evidence enters next decision; D3 then PASS |
| P4 | K-change/shell/unauthorized proposals rejected without execution |
| P5 | Four proposals: two valid, one duplicate, one unauthorized; only two admitted |
| P6 | Error/timeout: bounded fallback, no fabricated K failure |
| P7 | Restart after claim: no second producer invocation |
| P8 | Concurrent requesters: one proposal-round winner |
| P9 | UNKNOWN blocks producer and next attempt |
| P10 | Known FAIL/generated PASS with byte-identical ContractRef and unchanged K |
| P11 | Choice alone cannot create PASS; actual Verifier receipt required |

General LLM transport is a later separate PR. Reuse an existing abstraction if
suitable; otherwise add only a CandidateProducer-scoped adapter, not a generic
agent framework, workflow engine, vector database or computer-use loop.
