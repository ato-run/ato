# Formation 5a-a — DecisionProvider verification ledger (2026-09-26)

Implementation ledger, not a deployment record. Design: ADR-035. No deploy,
remote migration (0301), feature flag, workflow change or CI rerun.

## Scope

Requester-side DecisionProvider over finite AllowedChoices (frozen D ×
admissible untried placement, default first). One durable decision point per
attempt sequence; out-of-set, silence, provider failure and a spent decision
budget take the deterministic default under the same search budget. The
Coordinator never calls a model. `JevDecisionProvider` uses its own key
(`ATO_DECISION_JEV_API_KEY`), separate from the Browser judge.

## Local tests

- ato: formation 204 (search_decision 9 new), receipt authority 8 (decision
  authority 1 new), formation-worker incl. `decision_provider_v1` 6 new
  (Jev wire against a local HTTP stand-in: valid choice + evidence, question
  carries no offered data, 7 malformed/out-of-set answers, 500, timeout,
  oversized response, unreachable, oversized request never sent, key
  separation). A search without a policy keeps byte-identical frozen bytes and
  SearchState fixtures.
- Linux (oci-linux-test, aarch64): `ato-formation`, `ato-formation-worker`,
  `ato-receipt-authority` — 409 passed, 0 failed.
- ato-api: Runtime Network suites 132 (Stage 5a 8 new). The three 0301 fences
  (`trg_decision_blocks_issue`, `trg_decision_chosen_offered`,
  `trg_decision_write_once`) were each dropped once and the test failed.

## Actual acceptance

Actual Coordinator (Miniflare D1/R2, production routes, restarted as a
process), real Rust Runtime and requester (`formation_search` with the
acceptance provider), own owner `decision_acceptance`, on oci-linux-test with
the Stage 4 harness (`scripts/acceptance/formation-5a-decision.py`). 0301 was
applied to the continued local D1 after a state backup. D1 = a route whose
page has no `/health` (known K failure), D2 = the passing route.

| case | result | evidence |
|---|---|---|
| A0 no policy | PASS | D1 `01M3DWJ8WNB46CD3BDHPEA6GAX` fail → D2 `01M3DWJB76WP7MDVJ5XARXBZVG` pass; no decision rows; frozen policy has no `decision` |
| A1 provider picks D2 over default D1 | PASS | one attempt `01M3DWJCWF70DT7Q42103XXH8R` (D2) `fully_satisfied`, requester accepted; decision seq 0 `chosen` `cb7ac5fbfaa3840ca` ≠ default `c5f0826b88427383e`; `decisions_used` 1; provider asked once |
| A2 out-of-set answer | PASS | recorded `out_of_set`; default D1 `01M3DWJG1CDJJDGBNGNWGYKA56` then D2 `01M3DWJJ2YASCFZ3RS2DHKYDVS`; `decisions_used` 0 |
| A3 silence + Coordinator restart while open | PASS | open point and no ticket survived the restart; `timeout` recorded at the 15 s deadline; default D1 `01M3DWK2HFT8JHT9JYBCXQK04D` first, then D2 |
| A4 `max_decisions` 1 | PASS | one `chosen` point (D-fail-b first), then the default order with no second point: `01M3DWK5VD9FD2RDE4FWRTCVPK` → `01M3DWK7X46KSQ8Z61MF23CGWG` → `01M3DWK8HVQ2TE0FSGQVYFS7PP` pass |
| A5 Coordinator restart after the answer, before the issue | PASS | no point re-opened, one decision row; the recorded choice (D2) issued after restart as `01M3DWP5EAPPTDQ7CJPEZACD1A` and passed; provider asked once |

A5's first two runs failed in the harness (the hold trigger was created after
the answer had already been issued; then a stale `created.json` of the first
run was read). Both were harness defects, fixed before the passing run; the
product behaviour did not change between runs.

## Not verified

A real Jev call: no `ATO_DECISION_JEV_API_KEY` exists on the acceptance host.
The Jev wire is verified only against a local stand-in. Browser full E2E is
not claimed.

## Review hardening (2026-09-26)

Review blockers fixed in ato `f7132025` and ato-api `fd12caab`:

- **Deadline is authoritative.** `submitDecision` advances the search at the
  Coordinator's time before judging an answer, so an expired point is settled
  as `timeout` (and its default issued) first; `validate_decision` refuses an
  answer at or after `expires_at` (`decision_expired`); 0301
  `trg_decision_deadline` refuses any non-timeout outcome stamped at or after
  the stored `expires_at`.
- **The decision budget bounds provider use.** `decisions_used` is incremented
  when a point opens (invariant `decisions_used == points`); failed, invalid,
  out-of-set and timed-out points stay spent.
- **Provider-visible facts are requirement-scoped** — in the Coordinator's view
  and again in the Jev provider — instead of a `runtime.*`/`toolchain.*` prefix.
- **out_of_set** is answered 200 with its recorded outcome; malformed 400,
  closed or expired 409.

Tests: formation `search_decision` 10, receipt authority 8,
`decision_provider_v1` 7; Linux (oci) formation + formation-worker + receipt
authority 411 passed, 0 failed; API Runtime Network 136 (Stage 5a 12). Deadline
fence mutation-checked (dropped → the late-outcome test fails).

Actual acceptance rerun (same harness; 0301 recreated in the continued local
D1 after a state backup; A8/A9 under a second owner because the first used up
its 16-object source quota):

| case | result | evidence |
|---|---|---|
| A0–A5 | PASS | rerun on the hardened code; A2 `out_of_set` and A3 `timeout` now spend the decision (`decisions_used` 1) |
| A6 late answer | PASS | point opened with a 3 s deadline (`expires_at` 04:54:45.523Z); the provider answered D2 at +6 s; settled `timeout` at 04:54:48.815Z, never `chosen`; default D1 `01M3E11RCJN2YZSKWYY6WVR34S` first, then D2 `01M3E11TD8J8CG50ADQ4QDEFHK` |
| A7 provider failures spend the budget | PASS | `max_decisions` 1, provider errors: one `provider_error` point, no second point although 2 choices remained; default order `01M3E11W9QCR9P7J4FDXEAWGR3` → `01M3E11YBJE8BQSBE7X5K5BJEQ` → `01M3E11Z0TR07DAM794S2YZT0A` pass; provider asked once |
| A8 requirement-scoped facts | PASS | canary facts (`runtime.hostname`, `toolchain.secret_token`) injected into the Runtime's profile; the provider saw only `runtime.process`, `containment`, `toolchain.root` (the D's requirements) of 18 Runtime facts, no canary |
| A9 concurrency at the deadline | PASS | 12 choice POSTs and 12 status polls from −120 ms to +120 ms around `expires_at`: exactly one outcome (`chosen` by the POST at −120 ms, before the deadline), the other 11 POSTs 409, one ticket for the chosen D (`01M3E190RDEMR1AX1R55XEJP1Q`). The timeout-wins side of the race is A6 and the API race test |

## Scope and gates

This is **5a-a: finite choice of the next known-D attempt**. Inspection,
probe and stop choices are not implemented. The 5a completion gate — safety
with and without Jev, and a comparison of attempts, elapsed time and provider
usage/cost — is open, and no live Jev call has been made
(`ATO_DECISION_JEV_API_KEY` is not available on the acceptance host or
locally). 5a is not recorded as complete.
