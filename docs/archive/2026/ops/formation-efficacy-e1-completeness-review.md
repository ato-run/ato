# E1 aggregation completeness correction

Review base: PR #1417 head `e4ae9d3117003b553c416661d7e480c5d9d2e6b4`.
Scope: aggregation correctness only; no new efficacy experiment or hypothesis.

`complete` now requires equality between the actual cell-key set and the
preregistered E01–E10 × {0,1} × {A,B,C} domain. Duplicate keys remain errors.
A test also checks the fixed domain against the immutable original registration.

Verification:
- Before correction, missing-plus-unregistered substitutions failed the new
  regression for each of case, permutation and arm.
- After correction, **7 tests passed**: existing metric/gate tests, registration
  agreement, normal60, missing, each substitution, and duplicate rejection.
  Substitution tests make the other gate conditions true to isolate this fence.
- Reaggregated only `formation-efficacy-e1-results.json` into a temporary file.
  Output is byte-identical to the existing `formation-efficacy-e1-summary.json`:
  complete=true; A0/20, B9/20, C2/20; robust additional-success cases0;
  efficacy_gate_passed=false.
- No past observation, registration, summary or result record was rewritten.

No model calls, Runtime execution, model/context changes, deployment, remote
migration or merge. E1 is evaluation-complete with efficacy gate unmet; 5b-c
remains on hold. Neither B nor Jev v2 is promoted to a standard production path.
This correction returns the evidence-preservation PR for a separate merge decision.
