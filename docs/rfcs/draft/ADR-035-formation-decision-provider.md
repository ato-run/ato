# ADR-035 — Finite AllowedChoices DecisionProvider (Formation 5a-a)

Status: implemented on an unmerged PR pair (ato + ato-api); not deployed.
Migration 0301 is local-only. Builds on ADR-034 (SearchState) and ADR-026
(the decision provider is not the Browser judge).

Scope: this is **5a-a, finite choice of the next known-D attempt**. The rest of
5a — inspection, probe and stop choices — is not implemented, and 5a is not
complete: its gate (safety with and without Jev, and a comparison of attempts,
elapsed time and provider usage/cost, including a live Jev call) is open.

## Decision

A search may freeze an optional decision policy
(`policy.decision = {provider: "requester", max_decisions 1–64,
decision_timeout_ms 1 000–120 000}`). Without it nothing changes: the frozen
bytes, the canonical SearchState and every action are those of ADR-034.

With it, whenever the deterministic core's next action is an issue
(`IssueAttempt` / `ReplayRetained`) and at least two attempts could be issued
now, the core opens a decision point instead:

```text
decide_next(state, placements, now)
  default = ADR-034 action
  default is not an issue, < 2 choices, or decisions_used = max  → default
  no record for seq            → OpenDecision{seq, choices, default_id}
  open, before its deadline    → WaitForDecision{seq, expires_at_ms}
  open, past its deadline      → RecordFallback{seq, timeout}
  chosen and still issuable    → that choice's issue
  chosen, no longer issuable   → default   (event DecisionUnavailableAtIssue)
  any fallback                 → default
```

- **Finite set.** `allowed_choices` lists frozen D (safe effect classes only)
  × admissible, untried placements whose transfer fits the budget, in frozen
  D order and then the Coordinator's ranking, at most 32. The default is
  always the first. `seq` is the number of attempts the search has issued;
  `choice_id` is a stable digest label of (seq, D, placement). The provider
  can reorder, never extend: no new D, placement, argv, source change, stop
  or wait. The ADR-034 D-order wait for an unavailable earlier D is unchanged
  (a point opens only when the default is an issue).
- **Provider on the requester.** The Coordinator never calls a model and holds
  no provider key. The requester reads `decision_point` from the satisfy view
  and posts `POST /satisfy/:id/decisions` with a `choice_id` or a fallback
  reason (`invalid`, `provider_error`, `timeout`) and bounded evidence
  (≤ 16 KiB: model, confidence, probabilities, usage).
- **One durable answer per point, before its deadline.** The Rust authority
  (`validate_decision`, bounded WASM) judges an answer against the choices
  recorded when the point opened, at the Coordinator's trusted time. From the
  deadline (`opened_at + decision_timeout_ms`) on, only the timeout fallback
  may settle the point: the Coordinator first advances the search (settling an
  expired point as `timeout`), then judges the submission, and the database
  refuses a non-timeout outcome stamped at or after `expires_at`. A named
  choice that was not offered is recorded as `out_of_set` (200). The outcome is
  write-once; a later answer is `409 decision_closed`. After any restart the
  recorded outcome is reused; no provider is asked again.
- **The decision budget bounds provider use.** Opening a point spends one
  decision atomically, whatever its outcome (`decisions_used` = number of
  points). Silence, a malformed or out-of-set answer and a provider error all
  take the default and stay spent; at `max_decisions` no further point opens.
  Attempt/transfer/expanded/stored budgets, UNKNOWN blocking, owner stop and
  the issue-time placement re-check are unchanged, so a choice can never
  obtain more work than the default could.
- **K is untouched.** K is judged only by `verify_observed_candidate`, routes
  are accepted only by `accept_verified_route`. A choice is a scheduling
  preference, recorded as evidence, never as a verdict.

## Jev as the decision provider

`JevDecisionProvider` (ato `apps/formation-worker/src/decision_provider.rs`)
asks Jev (`POST https://api.typesafe.ai/v1/systemone`) one native Choice over
exactly the offered labels. Following ADR-026 it is separate from the Browser
judge: its key is `ATO_DECISION_JEV_API_KEY` (never `JEV_API_KEY`), its model
pin `ATO_DECISION_JEV_MODEL` (default `jev-1.13.0`). The instructions and each
criterion are fixed text naming only labels; offered derivation summaries,
runtime facts and prior typed failure codes are `state` data. Runtime facts are
scoped to the facts the choice's own D requires — by the Coordinator's view
and again by the provider — never by a naming convention, so an arbitrary or
future fact (a host name, say) is not sent. It sees no
receipt, verdict, source or secret. Requests over 48 KiB are not sent; an
answer is accepted only when its model is a `jev-` model, its choice is an
offered label and its probabilities cover exactly the offered labels. Any
other outcome is a fallback. The CLI enables it with
`ato ... --runtime-network --decision-provider jev`.

## Consequences

Gate 12 of ADR-034 ("no AI decision in the deterministic core") still holds:
the core is pure; a provider's answer is durable input to it, like an owner
resolution. Deployment, migration 0301 and any production provider key are
separate authorization gates. 5b (typed new-D generation) is not part of this.
