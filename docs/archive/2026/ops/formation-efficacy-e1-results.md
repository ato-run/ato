# Formation E1 efficacy results — 2026-09-27

**The proposed efficacy gate was NOT met. Overall Formation 5b remains In progress.**
This measures bounded parameter synthesis, not arbitrary program generation.
No 5b-c feature was started, no deployment or remote migration was performed.

## Integrated foundation and fixed evaluation base

All six PRs were merged using their explicitly approved heads, in order:

| PR | Approved head | Merge commit |
|---|---|---|
| ato #1412 | `1cefce635e573326d9b1905360791139ed7fa63e` | `23757b4854478d065a045893c1b7055c2b89231e` |
| API #696 | `ca4c3eebb1c97b2aba47e157bc849c69533453ca` | `b23d429c2f54d78b7c51116e2dff5f76d0035130` |
| API #697 | `39c4e7787f144c0bec94fe0bce5bd129597db94a` | `4b695432478084082cd55db3b1454ae50d49fb15` |
| ato #1413 | `5a0b37ecc05d37ff7bf19c874067b3b3a76b00c6` | `3e42f48a444a7aaac12af64b44040e1a774a2cf9` |
| ato #1414 | `8ebd97c34bdc78f89918c72ab12c85f16bfb9e03` | `6d15d51656a0f0ff9eacd8e2e56340c00d9cb95a` |
| ato #1415 | `987986d310dd1081772403f5e6a2b9b510d2bd64` | `beefc6101beae745cb9e258fd59ea7c2d8f65386` |

#1413 was made Ready before merging. Stacked bases were retargeted to main only
after their ancestors merged; no approved head was rebased or changed. Explicit
admin-merge approval was obtained after normal merge was policy-blocked.
[Machine-readable merge record](formation-5b-merge-2026-09-27.jsonl).

Evaluation base: ato `beefc6101beae745cb9e258fd59ea7c2d8f65386`,
API `4b695432478084082cd55db3b1454ae50d49fb15` (same receiver tree as reviewed
`39c4e778`). The requester adds only the evaluation selector to the acceptance
example; production Provider authority is unchanged. The actual Runtime binary
was reused from prior verified acceptance and pinned by digest, not rebuilt from
current main. Source and all executable digests are in the registrations.

## Protocol integrity and deviation

Original preregistration: commit `410285f3679a9e9f25b9787f96db1a7f24b3f10f`,
[plan](formation-efficacy-e1-plan.md) / [hashes and budgets](formation-efficacy-e1-plan.json).
10 cases × 2 opaque-ID permutations × A/B/C = 60 evaluated cells, 20 per arm.
Exact model `jev-1.13.0`, unchanged generation prompt `/2`.

**Comparator integration error and explicit amendment:** the initial B-only
validation used a model name rejected by the shared validator. Four completed B
cells were invalid and one B cell was interrupted; none are used as evidence of
Jev superiority. We stopped, repaired only that validator argument, added an
integrated provider regression, and pushed the
[amendment](formation-efficacy-e1-amendment.md) before continuation
(commit `990757eeec01c90deb97449df2c6826d909bd724`).
Original observations remain [separately preserved](formation-efficacy-e1-original-observations.json).

The four completed original C calls (two PASS, two decline) were reused exactly,
not repeated; only 16 unvisited C cells followed. All nine completed original A/C
cells were retained. New B used the *same preregistered score/tie-break rule*.
There were **20 unique C reservations / 20 model calls total**, no C retry,
cherry-pick, prompt tuning or fixture substitution.
[Reservation and original-file-digest audit](formation-efficacy-e1-reservations.json).
This is an **amended experiment**, not a pristine uninterrupted run. The repaired
schedule invalidates speed-comparison claims. Historical G9 and v1 remain untouched.

## Results

All success entries require actual Runtime execution, same frozen K, a fresh
`fully_satisfied=true` receipt and acceptance by the real Rust requester.
Every A/B/C comparison checked identical source hashes, K and budgets; B/C also
had identical provider-visible context. There are no missing final cells,
recorded execution-harness errors, input drift, Exact escapes, K changes,
UNKNOWN-generation escapes or repeated provider invocations.

| Metric (20 cells each) | A: no generation | B: deterministic selector | C: Jev v2 |
|---|---:|---:|---:|
| Same-K success | 0 / 20 (0%) | **9 / 20 (45%)** | **2 / 20 (10%)** |
| Generated D admitted | 0 | 18 (90%) | 8 (40%) |
| Admission / proposed draft | n/a | 18 / 18 | 8 / 8 |
| Decline | 0 (no provider) | 2 (10%) | 12 (60%) |
| Invalid / rejected proposals | 0 | 0 | 0 |
| Admitted D but execution K fails | 0 | 9 | 6 |
| Total attempts | 20 | 40 | 30 |
| Attempts-to-PASS, successful cells only | n/a | eight at 2; one at 3 | two at 2 |
| Local/provider invocations | 0 | 20 | 20 |
| Model calls | 0 | 0 | **20** |
| Input / output tokens | 0 / 0 | 0 / 0 | **22,292 / 1,488** |
| Estimated model cost | $0 | $0 | **$0.000936264** |
| Sum elapsed | 76.693 s | 200.193 s | 165.150 s |
| Median elapsed | 3.4985 s | 7.984 s | 7.007 s |
| Median provider latency | n/a | 0 ms | 113.5 ms |

Usage and cost are present for every C cell. Cost is an estimate at
[official pricing](https://docs.typesafe.ai/models), checked 2026-09-27:
$0.042/M input tokens, free output; not a billing statement. Neither elapsed
nor fewer attempts implies improvement when the success rate is lower. All
fixtures intentionally begin with known-D failure; A's 0% is not a real-world
known-D coverage estimate. E09's no-generation Exact early stop is documented
in the plan; B/C are matched at the same three-attempt ceiling.

[Per-cell safe ledger](formation-efficacy-e1-results.json) records selected IDs,
canonical D refs, K refs, safe fresh-receipt projections, contexts, usage/cost,
timing, report digests and outcomes. [Aggregate JSON](formation-efficacy-e1-summary.json)
is reproducible with `scripts/acceptance/efficacy/analyze.py`.

### Case and permutation results

Each pair is permutation0 / permutation1. FAIL includes valid decline or a
K-failing execution; it is never treated as an invalid proposal automatically.

| Case | B | C | C outcome |
|---|---|---|---|
| E01: one explicit handler vs CLI | PASS / PASS | PASS / PASS | admitted, same-K PASS both |
| E02: multiple correct handlers | PASS / PASS | FAIL / FAIL | decline both |
| E03: framework-marker decoy | FAIL / PASS | FAIL / FAIL | decline both |
| E04: CLI-looking correct wrapper | FAIL / FAIL | FAIL / FAIL | visible HTTP decoy admitted, K fails both |
| E05: identical lexical summaries | FAIL / PASS | FAIL / FAIL | decline both |
| E06: positive candidate too_large | FAIL / FAIL | FAIL / FAIL | visible wrong handler admitted, K fails both |
| E07: positive candidate unavailable | FAIL / FAIL | FAIL / FAIL | visible wrong handler admitted, K fails both |
| E08: process failure evidence | PASS / PASS | FAIL / FAIL | decline both |
| E09: actual failure inspection | FAIL / PASS | FAIL / FAIL | decline both |
| E10: no valid service / decline correct | FAIL / FAIL | FAIL / FAIL | appropriate decline both (B also declines) |

There are **zero additional C successes over B**, even before requiring both
permutations, and therefore zero robust additional-success cases (required ≥2).
The two C successes are exactly cells B also solves. E1 does not close 5b.

## Information insufficiency versus model value

Observed evidence supports specific limits, not a universal claim about Jev:

1. **Indistinguishability:** E05/E09 summaries are byte-equivalent after removing
   opaque IDs, although the health status literals differ. Those literals are
   deliberately absent. E09's real durable `attempt_failures` inspection reaches
   generation, but exposes another global `http_status_mismatch`, without an
   association to alternative entrypoints. It does not break the symmetry.
   Neither model nor deterministic selector can infer the omitted distinction
   from this context alone. B's single-permutation win is an ID tie-break artifact.
2. **Missing source semantics:** E04's wrapper delegates through runpy; the fixed
   lexical projection does not describe delegation. E06's positive source is
   too_large, E07's valid Latin-1 source is unavailable to UTF-8 projection. C
   selects the visibly HTTP-like wrong candidate in all six cells. More model
   selection over the unchanged lossy context did not repair these cases.
3. **Failure vocabulary loss:** E08's actual parent process failure projects to
   `other`, not a discriminative process-failure code. B's simple source rule
   succeeds twice, C declines twice. This identifies a concrete information loss,
   but does not prove that restoring a code would change the model's answer.
4. **Conservative selection is not an improvement:** C also declines both E02
   cells, where actual B runs prove both mapped candidates satisfy K. The model's
   internal reason is not observed. Full K is not supplied by prompt v2, so this
   experiment cannot disentangle model caution from insufficient goal/evidence
   representation. It does show lower measured utility than this fixed B rule.

No causal claim about source-context benefit versus v1 is possible without a
separate ablation, and no additional ablation was run. Before 5b-c, identify a
minimal typed, privacy-preserving fact that actually separates a failed case;
otherwise do not expand a repair agent or repeat prompts until success. Any
future schema/prompt/fixture change needs a new hypothesis and preregistration.

## Fixture-label validation and verification scope

Unselected positive candidates E04/E06/E07 were initially only syntax-checked.
A [separate post-outcome fixed-draft audit](formation-efficacy-e1-oracle-plan.md)
was registered before execution: three cells, **zero model calls**, no contribution
to A/B/C rates or timing. **All three fixed-draft audits passed**, with the same
K/source hash as their corresponding E1 cells and fresh receipt acceptance;
[separate audit results](formation-efficacy-e1-oracle-results.json). This confirms
that these unselected positives really can run, but cannot rescue the failed
efficacy gate or justify model retries.

Fresh local checks: worker **237 passed / 1 existing ignored**; acceptance example
**4 passed** (including integrated comparator); fixture test **1 passed**;
analysis tests **3 passed**; example Clippy and CLI check PASS. API code/schema
were not modified and its 220-test prior stack verification was not rerun here.
Actual integration used isolated Miniflare `stage5b-api` with already-initialized
local 0303, actual Rust requester and actual Rust Runtime, not a mock HTTP success.

## State separation

- **Implemented:** 5b-a/5b-b foundation; evaluation-only selector/harness/analysis.
- **Locally/integration verified:** prior minimal G9 gate plus amended E1's complete
  60-cell comparison. Efficacy threshold failed; this is not an implementation PASS claim.
- **Merged (post-merge status note, 2026-09-27):** six approved foundation PRs and
  evidence-preservation PR #1417 at `c9fc25fea5ed474c3d24ab64750645190b0f608a`.
  This status correction changes no E1 observation, aggregate, or efficacy conclusion.
- **Deployed:** no. No staging/production changes, remote migration, provider-key
  configuration changes, manual CI reruns, #1402 edits or 5b-c work.

Future deployment remains a separate gate, including remote 0299→0300→0301→0302→0303
and receiver-before-requester rollout. Nothing in this evaluation authorizes it.
