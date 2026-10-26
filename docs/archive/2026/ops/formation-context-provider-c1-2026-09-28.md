# Formation 5b-c1: context/2 provider integration, offline verification

Exact base: `660dbf6eaedc3384017137f1f20282a359d423aa` (origin/main after #1419).
Branch: `feat/formation-context-provider-v3`.
Design: [ADR-040](../rfcs/draft/ADR-040-formation-context-provider-v3.md).

## State separation

- **Implemented:** requester-local point/3, prompt/3, cached context/2 opt-in,
  existing one-shot claim and response authority reused.
- **Locally/integration verified:** offline request/response tests, loopback
  Coordinator mock + recording provider, full Rust regression and Clippy.
- **Merged:** c0 #1419 merged at the base SHA above; c1 is unmerged, review pending.
- **Deployed:** no; no staging/production changes, remote migration or key setup.

This is not a live Jev acceptance and not an actual-Coordinator/Runtime efficacy
run. Model calls **0**; Runtime efficacy reruns **0**; E2 **not started**. Synthetic
response usage tests do not create live token/cost observations.

## Provider-visible boundary

`GenerationPointV3`: exact schema, revision, expiry (<=64 bytes), claimed flag,
finite entrypoint IDs, context/2; whole serialized/raw decoder bound 18 KiB.
Context stays <=16 KiB, <=16 entries, <=1 KiB per summary, source reads <=65,536
bytes. ID-set equality is checked before claim and request/response validation.
Point/2+context/2 and point/3+context/1 are rejected.

Provider request `state` equals the validated context/2 and nothing else:
entrypoint opaque IDs and closed lexical/encoding/delegation/scan summaries,
project presence/count facts, fixed failure statuses/codes and safe inspection
summaries. Revision, expiry, raw failures/evidence, attempts, Runtime/host IDs,
receipt, source, filenames/paths, literal values, comments, URLs, argv and secrets
are not forwarded. Questions/criteria use the pre-existing finite domain and
fixed instructions; prompt/3 treats markers as evidence, not proof. No K/goal
summary, output authority expansion, heuristic preference or selector is added.

## Offline results

Existing candidate bytes are verified against E1 preregistered SHA-256 values.
No fixture code is executed and no E1 result is reaggregated.

| Gate | Result |
|---|---|
| E04, both opaque-ID permutations | 2/2 provider requests contain python_main delegation |
| E06, both permutations | 2/2 contain bounded_prefix + server evidence |
| E07, both permutations | 2/2 contain latin1 + complete scan evidence |
| E05 and E09, both permutations | summaries still equal except ID; no status-literal oracle |
| E10, both permutations | no invented server/delegation marker |
| V1 / V2 historical provider tests | all 24 unchanged tests pass |
| V3 privacy | whole request JSON excludes source/target-path/comment/secret/URL/argv/receipt/attempt/runtime/host/message canaries |
| Receiver injection | valid but contradictory context/2 and malformed receiver context are ignored |
| Source freeze | original directory mutation cannot alter enabled local context |
| Claim ordering | mock provider observes only successful claim's updated revision and claimed=true |
| Lost claim / completion response + restart | 0 / at most 1 recording-provider invocation, no second claim winner |
| Concurrent requesters | one claim winner and one recording-provider invocation/completion |
| Invalid domain, oversized point, malformed claim | no provider invocation; invalid local point makes no claim |
| Response authority | only offered python_script draft or decline; injected authority fields rejected |
| Provenance | exact model and prompt/3, bounded synthetic usage; no live usage |

E06/E07 metadata and evidence are not correctness proofs; negative candidates
also have service markers. These tests establish faithful transport of recovered
information, not improved selection or same-K success. E1 remains **B 9/20,
C 2/20, robust additional success 0, efficacy gate unmet**. 5b remains In progress.

## Regressions

```sh
cargo test --locked -p ato-formation -p ato-runtime-attempt -p ato-formation-worker -p ato-receipt-authority
cargo clippy --locked -p ato-formation -p ato-runtime-attempt -p ato-formation-worker -p ato-receipt-authority --all-targets -- -D warnings
```

**579 passed, 0 failed, 1 existing ignored**: formation 262, worker 249,
runtime-attempt 58, receipt-authority 10. V3 provider-request suite 6 tests;
requester suite 22 tests. Clippy passed with warnings denied. Local logs:
`.tmp/c1/regression.log`, `.tmp/c1/clippy.log`. No manual CI rerun.

## API and historical records

Read-only API audit at origin/main `f7d866cbef7768f67b46840766497fbf806640fe`:
`GenerationProvenanceSchema.prompt_version` accepts bounded strings; completion
wire schema is unchanged. Local point/3 is not receiver persistence. API changes
and API test reruns are unnecessary for this slice; no API PR. No migration was
created or applied, and `drizzle/0303_formation_generation.sql` was not changed.

E1 plans/amendment/reservations/observations/results/summary/oracle, evaluator
files, c0 offline observations and ADR-039 remain byte-identical to the base.
#1402 was not edited or merged. E2 preregistration awaits separately authorized
work after reviewed c1 merge fixes the exact artifact SHA. No automatic merge.
