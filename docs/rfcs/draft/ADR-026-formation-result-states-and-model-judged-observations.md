# ADR-026 — Formation result states; model-judged observations

**Status**: proposed
**Context**: the Formation roadmap after ato#1390 / #1389 (common attempt
entry; generic process). Before execution moves into a shared crate (stage
2a), two meanings have to be fixed, because every later stage reports them:
what a Formation result says it established, and what kind of judgment a
browser criterion is. Code today:
`lib/formation/src/{verify,request,receipt,browser}.rs`,
`apps/formation-worker/src/{attempt,job,local,runtime_network}.rs`.

## 1. Four outcomes, reported separately

A Formation attempt establishes up to four different things. They are
reported as four fields, never folded into one success flag.

| Outcome | Established when | Does not say |
|---|---|---|
| `seal` | The profile's sealing rule holds: no observation Failed, and every Deferred observation names the concrete gate that will decide it. | That any Run observation was verified. |
| `runtime_verification` | Every observation of the frozen K was Satisfied by this attempt, and the attempt's receipt is the evidence. | That another Runtime, or a later attempt, will satisfy K. |
| `cleanup` | The candidate was stopped and its scratch removed. | Anything about K. |
| `publication` | The artifact was kept (content-addressed store, bundle, upload). | Anything about K. |

Rules:

- A result with any Deferred observation is never called verified.
  `runtime_verification` requires the receipt's `fully_satisfied`; a seal
  that admits Deferred is only a seal.
- Later outcomes never rewrite earlier ones. A cleanup or publication
  failure after verification leaves the receipt and its verdicts as they
  were (as `run_attempt` already does with `candidate_cleanup_failed` and
  `artifact_store_failed`).
- None of the four is a boolean. Each distinguishes at least
  `not_attempted`, `succeeded`, `failed` and `not_applicable`, with a
  reason. In particular, when a verified candidate is handed off to a Run
  that keeps it (stage 2b, `HandOff`), cleanup is `not_attempted` with the
  reason `handed_off` — normal, not a cleanup failure — and the new owner
  records the eventual stop.
- `VerifiedRoute` is derived from `runtime_verification` only, and names
  the attempt whose receipt it rests on.

Where the code stands (not changed by this ADR):

| | Today |
|---|---|
| Local / Runtime Network (`run_attempt`) | `runtime_verification` from the receipt; cleanup and publication failures are separate failure codes on the attempt, not separate fields. |
| Hosted Formation job (`run_claimed_job`) | Seals when `verify()` reports no Failed observation. `verify()` emits Deferred only when the projected readiness gate observes the same port and path, so the gate is named; what that gate later decides does not come back into the Formation result. No `runtime_verification`. |

The fields land with the result types when execution moves into the shared
crate (stage 2a/2b) and the hosted result wire becomes
`ato.formation-result.v2` (stage 2d). Until then, attempts keep their
current failure codes.

## 2. Model-judged observations are their own evidence class

Two different things are called "Jev" in Formation:

- **Browser judge** (ADR-020): the pinned judge model (`jev-1.13.0`) rules
  each browser criterion `complete` / `incomplete` / `verify_more` on the
  observed page state. That ruling decides the criterion's verdict.
- **Formation decision layer** (roadmap stage 5a): a component that picks
  the next action from a finite `AllowedChoices` set. It never decides
  whether K holds.

This ADR keeps ADR-020's browser design and makes explicit what its
verdicts are:

- A browser criterion verdict is a **model-judged observation**. The
  browser snapshot it cites is observed, but whether the page satisfies the
  criterion is a model's judgment. It is not the same guarantee as an HTTP
  status or a digest comparison.
- `validate_result` and `accept_browser_receipt` check that the verdicts
  are consistent with their judgments, evidence and events, and recompute
  the overall verdict from the criteria. That makes the aggregation
  deterministic; it does not make the basis of each verdict deterministic.
  No document or UI may present a browser PASS as deterministically
  verified.
- Receipts and verified routes carry the class: a browser receipt states
  that its criteria are model-judged and which judge model and version
  judged them. A route whose effective K includes a browser Contract shows
  that part as model-judged next to the deterministic observations.
- The two roles get different names and different configuration: the
  browser judge stays the judge (its model pin, keys and rounds are the
  verifier's); the stage 5a component is a decision provider and has no
  access to the judge's configuration or to K's verdicts.

The receipt field (for example `judgment: "model"` with the judge model
required) is added with the next browser-receipt change; a receiver
continues to refuse a browser receipt it cannot re-validate.

## Consequences

- Stage 2 has a fixed vocabulary for what a Formation, a Run and a hosted
  job report; the hosted wire v2 carries the four outcomes instead of a
  `status: succeeded`.
- A browser-contract route is never shown as more certain than it is, and
  the decision layer cannot be mistaken for a verifier.
