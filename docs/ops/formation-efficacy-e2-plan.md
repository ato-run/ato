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
  authority/K/UNKNOWN/permission/Runtime escapes; zero repeated model calls;
  zero errors and zero protocol violations.
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
apps/formation-worker/fixtures/runtime-network/e2-holdout/. No E1
candidate/wrapper/padding/encoding-cookie/private-source bytes are reused.
Shared non-decision scaffold (notes app.py, capsule.toml, index.html, etc.)
may be reused and is identical across arms. Fixture generation never branches
on arm or expected selector outcome.

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

The oracle asks: **Does this fixed candidate produce the preregistered
terminal same-K outcome under the actual Runtime?** It reads the exact 24
cells in plan.json, permutation0, providers fixed:k4/fixed:v9, the same frozen
source/K/budget and pinned requester/Runtime/WASM/API. Each cell has one
`expected_result` from a closed enum; no alternative outcome sets are allowed.

- `verified_pass`: admitted generated D, actual finished attempt, fresh same
  K/D/request/attempt receipt with fully_satisfied=true and requester-accepted
  verified route. This is the unchanged primary success definition.
- `verified_k_fail`: admitted generated D, actual finished attempt, fresh same
  K/D/request/attempt receipt with fully_satisfied=false, no verified route,
  requester exit 0 and unsatisfied.
- `terminal_not_observable`: exactly one admitted non-parent generated D and
  its actual attempt; request/search/attempt/Runtime/K/D identities agree;
  execution_started=true, attempt_record=finished, fail status and the exact
  candidate_not_observable verification failure; no receipt/verification,
  verified route or UNKNOWN; cleanup succeeded and realization.destroyed=true;
  requester exit 0 and unsatisfied. Process realization must name app.http:
  the pinned launcher populates this evidence only after successful launch,
  excluding setup/launch errors that can share the same failure code.

Receipt absence, raw error text, a generic process/setup failure or timeout
alone proves no oracle class. formation_failed, dependency_unavailable,
runtime offline, unfinished records and UNKNOWN remain protocol failures.
A terminal_not_observable proof matches only cells registered for that class;
primary records it as a normal failure, never in the success numerator.

### Static evidence audit (no candidate or Runtime execution)

The fixed python_script route runs the candidate as a script, with no inferred
Flask server command or dependency-install step. Each of the 24 classes below
is registered from the committed source and Runtime control flow only:

| Case | k4 / candidate_0 | v9 / candidate_1 | Static reason |
|---|---|---|---|
| C01 | verified_pass | terminal_not_observable | HTTP 200 server / print and exit |
| C02 | verified_pass | verified_pass | Both HTTP servers return 200 |
| C03 | terminal_not_observable | terminal_not_observable | Both print and exit |
| C04 | verified_pass | verified_k_fail | READY=True 200 / READY=False 503 |
| D01 | verified_pass | verified_k_fail | runpy target serves 200 / /health 503 |
| D02 | verified_pass | verified_k_fail | runpy target serves 200 / static /health 404 |
| D03 | verified_pass | terminal_not_observable | runpy target serves 200 / Flask app never starts a server |
| L01 | verified_pass | verified_k_fail | Latin-1 HTTP 200 / static /health 404 |
| L02 | verified_pass | terminal_not_observable | Latin-1 HTTP 200 / Flask app never starts a server |
| P01 | verified_pass | terminal_not_observable | HTTP 200 plus comment padding / print and exit |
| P02 | verified_pass | verified_k_fail | HTTP 200 plus comment padding / /health 503 |
| P03 | verified_pass | terminal_not_observable | HTTP 200 plus comment padding / Flask app construction only |

There are 12 verified_pass, 5 verified_k_fail and 7 terminal_not_observable
cells. The three Flask-looking scripts have no server invocation whether the
import succeeds or the script exits on an unavailable import. A declared
dependency-resolution failure remains a different typed Runtime outcome and
cannot satisfy the oracle. All other imports are stdlib; delegated targets
are committed; Latin-1 cookies and comment-only padding are explicit. The
static file decoys have no health file in the registered source tree.

In `lib/runtime-attempt/src/attempt.rs`, observe_http detects exit before
observation, not_observable leaves verification/receipt absent and records
cleanup; run_reserved_attempt writes the finished journal state. Process
launch evidence comes from `formation_realizer.rs`. The pinned worker's
report_for_attempt carries that FormationAttempt and typed attestation in
the request-scoped satisfy response. A failed HTTP observation comparison
instead reaches verify_observed_candidate and a false receipt. No Runtime
semantics or candidate bytes are changed by this oracle amendment.

Any class mismatch or preflight failure stops the run with its reserved cell
saved. Output schema `ato.formation-efficacy-e2-oracle/2` stores expected_result,
observed_result and expectation_matches per row. Observed classes are the
three terminal classes above plus protocol_failure. Primary preflight
reconstructs observed_result from persisted evidence and requires exact
agreement with both the registered expected_result and the saved observed
class; summary fields alone cannot authorize the run.

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


## Prospective hardening (before any execution)

The amendment hardens protocol/integrity only; case bytes, oracle labels,
72-cell order, budgets, model/prompt/artifact pins and comparative gate
thresholds remain unchanged. See `hardening` in plan.json for exact fields.

Success now requires error=None, requester exit 0, satisfied status, exactly
one admitted generation row and its non-parent generated D in an actual
attempt. The PASS receipt must name that D, the frozen/result K and the fresh
request/attempt. There must be exactly one verified route, naming the same
D/attempt/Runtime/environment and receipt. The pinned requester's exit 0
means Rust accepted at least one route; requiring this sole route makes the
acceptance specific to the generated D without duplicating Rust verification.

The primary and oracle share `efficacy/e2_protocol.py` and
`efficacy/e2_execution.py`. Harness/setup/transport/timeout/drift/missing
telemetry errors persist the reserved cell then stop. Invalid choices with
valid model metadata, declines and admitted K failures remain efficacy
outcomes. Exact arm invariants include one provider call, A zero/B-C one
model call, singleton model/prompt lists and mandatory context schema. B/C
require observed input/output usage and cost; missing values remain unknown.
The frozen budget is compared to the registration, not merely across arms.

`efficacy/e2_oracle.py --run` reads only the 24 registered oracle cells. It
accepts no key and uses fixed:k4/fixed:v9, permutation0, context/1 (the existing
fixed-provider path). It checks the preregistered evidence class above;
receipt-free failures match only terminal_not_observable with full typed
identity, finished-journal, launch and safe-cleanup evidence. No E2 candidate
has been executed to choose or validate these classes.

The primary `--run --oracle-result <oracle-result.json>` validates the exact
plan SHA, environment pins, all 24 keys, zero model calls, completion and all
expectations before reading stdin or reserving any cell. It rechecks saved
evidence rather than trusting summary flags. Source/K/budget match the
oracle and all arms/permutations of each case. Output directories are
exclusive; reservations are exclusive and fsynced before setup. No resume
or retry is implemented.

Every registered code file and the registration must equal committed HEAD
bytes; all hashes and fixtures are checked before helper imports, then again
before and after each cell. API_DIR must be the Git checkout root at
`f7d866cbef7768f67b46840766497fbf806640fe`, with unchanged source/config and
actual tracked file hashes matching that HEAD. No automatic main update is
performed. The local Coordinator launch bundle is unavailable here, so no
bundle pin is invented: the registered checkout method is used. Actual
coordinator.mjs + worker-final-bundle hashes are additionally recorded and
must remain identical between oracle and primary. The execution environment
must be prepared separately; this PR does not start it.

Offline verification of the hardening: Rust regressions 583 passed / 0 failed /
1 existing ignored; formation_search example 5 passed; targeted all-targets
Clippy with -D warnings passed. Python E1/E2 fixture, analysis, protocol and
mocked orchestration tests: 91 passed (the previous 75 plus 16 evidence-class tests), including 20 E2 analysis tests. Mocked
oracle loop tests are not Runtime oracle executions. Model calls=0, oracle
executions=0, E2 Runtime cells=0.
