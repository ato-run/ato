# Formation 5a — DecisionProvider verification ledger (2026-09-26)

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
