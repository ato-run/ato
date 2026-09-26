# ADR-036 — Finite exploration actions (Formation 5a-b)

Status: implemented on an unmerged PR pair (ato + ato-api); not deployed.
Migration 0302 is local-only. Builds on ADR-034 (SearchState) and ADR-035
(the requester-side DecisionProvider). ADR-035 stays the record of the
attempt-choice mechanism; this ADR generalizes the offered set.

Scope: **5a-b, finite exploration actions**. A decision point no longer
offers only the next known-D attempt:

```text
AllowedChoices = Attempt | Inspect | Stop
```

The provider still answers with one `choice_id`; it can never write an
action. No new D, K, permission, Runtime, argv, source change, shell
command, URL or probe argument is reachable through it.

## Decision

- **A point's sequence is its own.** Under 5a-a `seq` was the attempt
  count, which worked because every settled point issued an attempt. An
  Inspect issues nothing, so `seq` is now the number of decision points
  opened before it, and `attempt_seq` records the attempt count at open
  — it is what tells "the released action is still pending" from "the
  search moved on". Migration 0302 adds `attempt_seq` to
  `satisfy_decisions`; existing rows keep their old seq as
  `attempt_seq` (it was the attempt count) and are renumbered densely.
- **`choice_id` identifies the action.** It is a stable digest of
  (seq, typed action). Mutable availability, ranking and descriptions are
  never part of it.
- **Inspect** is a Coordinator-side, read-only, bounded, deterministic-input
  producer over durable rows — never a shell, a network call or a
  provider-supplied argument. Two kinds exist:
  - `candidate_refusals`: the typed hard-filter reasons that keep a D
    off its placements (`satisfy_candidates` rows, ≤ 32 entries ×
    ≤ 16 typed reasons ≤ 1 KiB each).
  - `attempt_failures`: the typed failure summaries of a D's finished
    non-pass attempts (≤ 16 entries; status, failure code/stage, message
    ≤ 512 bytes; never raw logs). Offered only for a D that has one.
  Its result is durable evidence (`satisfy_evidence`, write-once per
  (kind, target), `ato.formation-inspection/1`): once recorded it is
  reused after any restart and never re-asked. Evidence already recorded is
  not offered again, so the offered set stays finite and shrinking.
- **Stop** is an explicit finite choice (`reason_class =
  no_promising_action`). It is always offered, is never squeezed out by
  the 32-choice bound, and ends the search as `decision_stopped` — a
  new terminal status on the stop row, not a K failure, not a Verifier
  verdict, and not one of the durable `termination_reason` values (the
  satisfy view reports `decision_stopped` from `stop.kind`).
- **Probe is deferred.** The candidates that fit this model — re-reading a
  Runtime requirement fact, checking toolchain availability — have no safe
  existing primitive the Coordinator can run: a runtime probe would need a
  new ticket surface, and advertised facts are already re-read on every
  advance. No Probe action exists until a typed, bounded, side-effect-free
  primitive does; nothing generic or provider-shaped may stand in for one.

## Flow

```text
action selected (chosen or default)
  attempt  → the recorded issue, once        → next point
  inspect  → bounded Coordinator inspection  → durable evidence → next point
  stop     → finish decision_stopped
fallback outcome → the deterministic default, exactly once → next point
```

A point opens only while the deterministic default is an issue and under
the same budget rules as 5a-a: < 2 choices or `decisions_used =
max_decisions` takes the default; each opened point spends one decision
whatever its outcome. Attempt, transfer, expanded, stored and deadline
budgets, UNKNOWN blocking, owner stop and the issue-time placement
re-check are unchanged. Past points, decisions and evidence are
append-only; a Coordinator restart replays the same frontier — a settled
point releases its recorded action exactly once, and recorded evidence is
never recomputed.

## Database fences (migration 0302)

- A point opens only at the search's own decision sequence and its current
  attempt count, with a running request, under `max_decisions`, with
  unique `action`-typed choices containing the default — and never
  while an earlier point's released action is still pending (open answer,
  unissued attempt, unrecorded inspection, or a chosen stop).
- No attempt inserts past an open point, a chosen stop, or a chosen
  inspection whose evidence is not recorded.
- Evidence is admitted only as the released action of the search's latest
  decision point (`evidence_not_chosen`), only at the search revision
  it was computed from and only while a request runs (silent drop), is
  write-once by primary key, and may never be updated
  (`evidence_write_once`). Recording it advances the search revision.

## Provider-visible boundary

Unchanged from 5a-a: the provider sees the offered `choice_id`s and
one bounded summary per action — for an attempt the D's own summary, its
requirement-scoped Runtime facts and prior typed failure codes; for an
inspect the kind and target; for a stop the reason class. Evidence reaches
the question as a separate bounded projection (typed reasons, failure
code/stage/status per Runtime) — never attempt ids, messages, receipts,
source, logs, secrets or unrelated Runtime facts.

## Consequences

ADR-035's attempt choice is one case of this action set; its privacy
boundary, fallback rule (same SearchState, same permissions, same
remaining budget → deterministic default) and one-answer-per-point
durability are unchanged. Deployment, migration 0302 and any production
provider key remain separate authorization gates. 5b (typed new-D
generation) is not part of this.
