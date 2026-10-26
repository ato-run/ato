# E1 preregistration — bounded Python generation efficacy

Registered before **any E1 model or Runtime experiment**, 2026-09-27.
[Immutable machine-readable registration](formation-efficacy-e1-plan.json)
fixes main SHAs, harness commit, binary/source/fixture hashes and all budgets.
Historical G9 and v1 decline remain unchanged; this is a new experiment.

## Question and gate

Does unchanged Jev prompt v2 add same-K successes beyond a simple deterministic
selector operating on exactly the same bounded context and candidate domain?
This is bounded parameter synthesis, not code generation or a general repair agent.

Close the proposed efficacy gate only if **at least two distinct cases** have C
same-K PASS and B non-PASS in **both ID permutations**, aggregate C successes
exceed B, and there are no authority/K/UNKNOWN escapes. Missing/invalid/declined
cells are never removed from the denominator. Passing admission is not success:
actual execution, same frozen K, fresh receipt and requester acceptance are required.
This small synthetic set does not establish broad statistical generalization or
all evidence-based D improvement. Stage 6 coverage is separate.

## Fixed arms and ordering

- A: known D only, generation policy absent. Existing no-policy Exact behavior
  remains unchanged (stops after first failed attempt, even in E09).
- B: evaluation-only `deterministic_v1`, using the same production-validated
  context as C. Only complete scans; score +4 custom HTTP handler, +2 listen,
  +1 HTTP/framework import (http_server/flask/fastapi/uvicorn/aiohttp/tornado/
  wsgiref/django), +1 main guard. Decline if no positive score. Descending score,
  then ascending opaque ID. Failure/inspection fields are available but this
  simple rule has no candidate-linked evidence rule. No oracle fallback.
- C: production Jev v2 provider, exact `jev-1.13.0`, unchanged prompt `/2`.

10 cases × 2 permutations × 3 arms = **60 cells**, at most **20 model calls**.
For each case, p0 uses A/B/C and p1 uses C/B/A. Every C cell gets at most one
invocation regardless of decline, invalid output, model error or execution failure.
No retries, tuning or cherry-picking. A and B have zero model calls.
IDs `q7`/`m2` swap candidate0/candidate1; filenames, source bytes and K do not.
Each arm uses its own owner-scoped Exact Runtime with identical advertised
capabilities. Same per-cell budget, source, K and initial known Ds; generation
policy is intentionally absent only in A. Compare B/C context byte-equivalent
JSON, cross-arm K and budgets; stop on drift.

## Cases frozen before outcomes

| Case | Alternatives | Intended success indices |
|---|---|---|
| E01 | Explicit HTTP handler vs CLI-only | 0 |
| E02 | Two working handlers | 0,1 |
| E03 | Working handler vs fixed framework-marker decoy returning wrong health status | 0 |
| E04 | CLI-looking runpy wrapper vs wrong-status HTTP handler | 0 |
| E05 | Same lexical summary; only health status literal differs | 0 |
| E06 | Working candidate exceeds 64KiB; visible wrong-status handler | 0 |
| E07 | Valid Latin-1 Python makes UTF-8 projection unavailable; visible wrong-status handler | 0 |
| E08 | Parent exits with process failure; custom handler vs static HTTP handler | 0 |
| E09 | Two known failures, decision sequence Attempt → attempt-failure Inspect → Attempt; same-summary alternatives | 0 |
| E10 | Two CLI-only candidates; no solution, decline preferred | none |

Labels are only in this evaluation plan, never provider input. E08/E09 test
**hypotheses** of failure/inspection usefulness, not proven useful evidence.
The existing projection drops target/attempt identities and full K; it may be
information-theoretically insufficient. E09 may expose duplicated/global failure
status rather than discriminative evidence. Record that result; do not augment
context or redefine the fixture to force a win. A's no-policy Exact early stop
is an explicit execution-count asymmetry, not missing data. B/C are matched.

## Metrics and audit

Per cell record success, draft/admission/decline/invalid or rejection, attempts
and nullable attempts-to-PASS, provider/model calls, exact model/prompt, usage,
estimated cost, provider latency, total elapsed, selected ID, D ref, K and safe
receipt projection. Record failures/missing usage without imputing tokens.
All rates use 20 cells per arm; admission conditional on draft is separate.
Cost estimate uses [official pricing](https://docs.typesafe.ai/models), checked
2026-09-27: $0.042/M input, free output; estimate, not invoice.

Source facts are frozen locally by the actual Rust requester. No receiver-made
context, source text, paths, keys or oracle labels are fed to the provider.
Exclusive experiment directory and persistent cell reservations prohibit reruns;
never reset them following ambiguous completion. Infrastructure failure is a
recorded missing/failed cell, not a retry permission. Stop on safety/input drift.
The record's artifact digests are checked before any provider process.

No deployment, remote migration or additional Formation production feature.
All six foundation PRs were merged in the approved order. Future rollout still
requires 0299→0300→0301→0302→0303 on the appropriate remote environment separately.
