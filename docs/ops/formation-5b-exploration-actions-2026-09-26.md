# Formation 5a-b — finite exploration actions verification ledger (2026-09-26)

Implementation ledger, not a deployment record. Design: ADR-036. No deploy,
remote migration (0302), feature flag, workflow change or CI rerun.

## Scope

`AllowedChoices = Attempt | Inspect | Stop` over the same durable
decision points as 5a-a. `seq` is now the point's own sequence
(independent of the attempt count); `attempt_seq` records the attempt
count at open. Inspect produces write-once durable evidence
(`satisfy_evidence`) from a bounded Coordinator-side read-only
producer; Stop ends the search as `decision_stopped` (not a K failure,
not an owner stop). Probe is deferred — no safe existing primitive exists.

## Local tests

- ato: ato-formation 211 (search_decision 16 incl. new inspect/stop/seq
  separation), ato-formation-worker incl. `decision_provider_v1` all
  pass, ato-receipt-authority 8, ato-runtime-attempt pass.
- ato-api: Runtime Network suites 93 + 47 (Stage5b 4 new): inspect evidence
  + new seq, `attempt_failures` projection, `decision_stopped`,
  and the 0302 fences (released-action evidence admission,
  `evidence_not_chosen`, stale-revision drop, write-once,
  pending-release blocks on issue and on the next point).

## Actual acceptance

Actual Coordinator (Miniflare D1/R2, production routes, restarted as a
process), real Rust Runtime and requester (`formation_search` with the
acceptance provider; new `kind:<kind>` and `seq:N=I,...` modes,
plus direct `POST /decisions` answers from the harness for
restart-controlled sequencing), own owner `exploration_acceptance`, on
oci-linux-test (`scripts/acceptance/formation-5b-exploration.py`).
Migration 0302 was applied to the continued local D1: it rebuilt
`satisfy_decisions` (the write-once trigger rightly refuses to update
recorded points), preserved the 5a rows with `attempt_seq` = old seq,
renumbered them densely, and re-asserted the decision triggers the pre-merge
0301 variant had shaped differently.

| case | result | evidence |
|---|---|---|
| B0 no policy | PASS | D1 fail → D2 pass; no decision rows, no evidence, frozen policy without `decision` |
| B1 attempt vs attempt (5a-a regression) | PASS | D2 chosen at seq 0, one attempt `01M3ED1CS4B5QDVSVJWFKY5Y0T` `fully_satisfied`; 1 provider call |
| B2 Inspect → restart → new seq → Attempt → PASS (B5+B6) | PASS | seq 0 chosen `candidate_refusals` → evidence row (5 typed refusals) with 0 attempts; Coordinator restart; evidence reused unchanged; seq 1 opened; attempt (D2) chosen → PASS `01M3ED1JZ738SA222ZJ2Q2YQSH` |
| B3 Stop | PASS | `kind:stop` chosen; 0 tickets; status `stopped`; `termination_reason=decision_stopped`; `stop.kind=decision_stopped` |
| B4 provider failure after Inspect | PASS | `seq:0=2` (inspect) then no script: outcomes `[chosen, invalid, invalid]`; deterministic default D1 fail → D2 pass; `decisions_used` 3 |
| B7 concurrent answers | PASS | 6 racy POSTs: exactly one 200, rest 409; released action is the winner's; no duplicate decision or evidence; settled `satisfied` |
| B8 provider-visible payload | PASS | canary facts (`runtime.hostname`, `toolchain.secret_token`) never in provider calls; inspect/stop choices carry empty `view_facts`; evidence projected without `attempt_id`/`message` |

Also observed in the durable rows: seq 2 of B4 opened at `attempt_seq=1`
after the first issued attempt, and its offers included
`attempt_failures` for the failed D — the second inspection kind
gated on real failures as designed.

## Follow-up (2026-09-26)

The PR pair merged: API #695 `1853f280` → ato #1410 `c883087e`.
At the original B0–B8 run no `ATO_DECISION_JEV_API_KEY` existed on the
acceptance host. The later [live Jev paired acceptance](formation-live-jev-2026-09-26.md)
used a process-only key from the user-designated ato-api vars and passed,
closing the remaining 5a live/comparison gate. Probe remains deferred (no
safe existing primitive). Deploy, remote migration 0302, staging/production:
not performed.
