# E2 preregistration — prospective holdout efficacy evaluation

Registered before **any E2 model call, Runtime efficacy cell or oracle run**,
2026-09-28. [Immutable machine-readable registration](formation-efficacy-e2-plan.json)
fixes the ato/API SHAs, harness commit, artifact/prompt/code/fixture digests,
the exact ordered cell-key set, all budgets and the gate.

E1 was development evidence used to design c0/c1; its fixtures (including
E04/E06/E07) are **never** used in E2 numerators or denominators and are kept
only for historical/privacy/regression use. E2 is not a retry of E1: it asks a
new question on a new prospective holdout with a new comparator.

## Question and gate

Does the merged context/2 + prompt/3 path produce more same-K successes than
(a) a fixed deterministic selector over the same closed typed context/2, and
(b) the identical pinned model over point/2 + context/1 + prompt/2? This is
bounded entrypoint selection after a known-D failure, not code generation or a
general repair agent.

The gate is fixed now and fails unless **all** hold:

- **Safety**: the recorded cell-key set equals the exact registered set; zero
  authority/K/UNKNOWN/permission/Runtime escapes; zero repeated model calls.
- C same-K success cells > A.
- C same-K success cells > B.
- At least 2 holdout cases where C passes both permutations and A fails both.
- At least 2 holdout cases where C passes both permutations and B fails both.

If any leg is unmet the gate is false; speed or cost never rescue it. Decline,
invalid, rejected, execution-K-failure and missing cells stay in every
denominator; unexecuted or missing cells are never successes. Model choice,
draft admission and HTTP startup alone are **not** success: only actual
generated D -> actual Runtime execution -> fresh receipt -> same frozen
effective K -> fully_satisfied=true -> requester acceptance counts. A passing
gate demonstrates bounded Python entrypoint selection efficacy only; it does
not complete general repair or Formation 5b.

## Arms and ordering

- **A**: deterministic_v2, evaluation-only selector efficacy-selector/2
  over the closed typed context/2 through point/3. Score =
  4*custom_http_handler + 2*server_listen + 2*(delegation==python_main)
  + 1*(any of http_server,flask,fastapi,uvicorn,aiohttp,tornado,wsgiref,
  django import markers) + 1*main_guard. Decline when no candidate scores
  positive; descending score then ascending opaque ID for ties. It reads no
  source bytes, paths, filenames or oracle labels; encoding and source_scan
  are provenance and carry no score. Frozen before outcomes; never a
  production default.
- **B**: Jev provider jev_v2, point/2 + context/1 + prompt/2, model
  jev-1.13.0.
- **C**: Jev provider jev_v3, point/3 + context/2 + prompt/3, model
  jev-1.13.0. B and C share the exact same pinned model via
  ATO_GENERATION_JEV_MODEL; changing the model requires a new registration.

12 cases x 2 permutations x 3 arms = **72 cells**, at most **48 model calls**
(24 B + 24 C). Order is the registered cell_keys sequence: case ascending
C01..P03; permutation0 runs A,B,C and permutation1 runs C,B,A. Every model
cell gets at most one invocation regardless of decline, invalid output,
provider error, lost response or execution failure; no retries, tuning or
cherry-picking. Opaque IDs k4/v9 swap candidate_0/candidate_1 file mapping
only; filenames, source bytes and K do not change.

## Holdout fixtures frozen before outcomes

New committed files under
apps/formation-worker/fixtures/runtime-network/e2-holdout/; every byte is
new. No E1 candidate/wrapper/padding/cookie bytes are reused and no existing
E1 application copy is relabeled. Fixture generation never branches on arm or
expected selector outcome.

| Case | Family | candidate_0 | candidate_1 | Intended correct |
|---|---|---|---|---|
| C01 | control | working HTTP site server | CLI-only script | 0 |
| C02 | control | working notes server | working health server | 0,1 |
| C03 | control | CLI report | CLI reindex | none (decline preferred) |
| C04 | control | indistinguishable READY=True server | identical-summary READY=False server | 0 |
| D01 | delegation | runpy wrapper to notes_app.py | HTTP handler, /health 503 | 0 |
| D02 | delegation | runpy wrapper to site_server.py | static file server, /health 404 | 0 |
| D03 | delegation | runpy wrapper to health_service.py | Flask decoy | 0 |
| L01 | latin1 | `# coding: iso-8859-1` HTTP server | static file server | 0 |
| L02 | latin1 | `# -*- coding: latin-1 -*-` site server | Flask decoy | 0 |
| P01 | prefix | >64KiB notes server, evidence in prefix | >64KiB CLI decoy | 0 |
| P02 | prefix | >64KiB site server, evidence in prefix | HTTP handler, /health 503 | 0 |
| P03 | prefix | >64KiB health server, evidence in prefix | >64KiB Flask decoy | 0 |

Labels live only in this registration, never in provider input. Every cell
starts from one known-failure parent D (bad.py, /health 404) and offers the
same frozen K (GET /health -> 200, GET / -> 200, captured workspace digest).

## Oracle plan (registered, not yet executed)

Before any model call, a zero-model audit verifies the intended labels against
the actual Runtime: 24 fixed cells, permutation0, providers fixed:k4 and
fixed:v9 on the same frozen source digests, original K, two-attempt budget,
pinned requester/Runtime/WASM. Actual execution -> fresh receipt ->
fully_satisfied -> requester acceptance must equal each registered
expected_fully_satisfied in plan.json -> oracle.cells, including negative
expectations where case semantics require the decoy to fail. Any oracle or
preflight failure stops the run; fixtures are then fixed in a new
commit/version, never silently continued. Once a model call begins, fixture
changes are prohibited. The no-generation path needs no primary arm; the
oracle also sanity-checks expected failure behavior.

## Cells, isolation and budgets

Each cell uses a distinct owner and search_id, the same pinned
requester/Runtime/receiver schema, identical source bytes and frozen K, one
known failing route, max_attempts=2, max_generations=1,
generation_timeout_ms=30000, provider_timeout_ms=20000,
harness_settle_seconds=180, harness_wait_seconds=240, and the registered
budget caps (deadline_seconds=604800, transfer/expanded/stored = 10 GiB).
Retained routes and past searches are never reused; per-cell environments are
scrubbed of ATO_ACCEPTANCE_* and Jev keys before the arm configuration is
applied. A persistent .reserved marker is written before each requester
starts and is never removed; a lost response authorizes no second call. The
experiment directory efficacy-e2 is created once; there is no resume.

## Metrics and audit

Per cell record same-K success, draft/admission/decline/invalid or rejection,
attempts and nullable attempts-to-pass, provider calls, model calls, exact
model, prompt version, usage, estimated cost, provider latency, elapsed,
selected opaque ID, derivation ref, contract ref, fixture hash, context schema
hash and safe receipt projection. Missing usage is recorded, never imputed.
All rates use 24 cells per arm; admission conditional on draft is separate.
Elapsed comparisons are invalid if any schedule or protocol deviation occurs.

Cross-cell checks compare per-case/permutation fixture hash, contract ref and
budget across arms, and the B/C provider records must carry the same pinned
model and their registered prompt versions. A deterministic A cell still emits
one provider call and zero model calls.

Completeness means the actual recorded cell-key set equals the registered
cell_keys set exactly. Duplicate, unknown or substituted cases,
permutations or arms are errors; count-only matching never closes the gate.

## Protocol deviations

A harness or comparator bug, environment problem or provider outage stops the
run; observed ledger rows stay immutable. A prospective amendment commit may
resume only unvisited cells; visited or reserved model cells are never re-run.
Amendments never retro-edit this registration.

## Historical preservation

E1 preregistration, amendment, reservations, observations, results, summary,
oracle artifacts and c0 offline observations are unchanged. E1 remains
B 9/20, C 2/20, robust additional success 0, efficacy gate unmet; Formation 5b
stays In progress. This work introduces no new authority: no Adapter,
permission, K projection, prompt edit or production selector is added.

## Status boundary

This registration fixes intent only. Zero model calls, zero Runtime efficacy
cells and zero oracle executions have been performed. The experiment itself
requires separate approval after this preregistration merges: first the
zero-model oracle audit, then the 72-cell run.

