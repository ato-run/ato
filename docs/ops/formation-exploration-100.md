# Formation exploration: frozen 100 OSS arm

100/100 actual terminals. Baseline **7/100** → exploration **7/100**, absolute gain **+0**. No new functional acceptance.

Runtime code `10473525efa26416ef63025dbb5003c910a43c70`, receiver `b20bba7ab9daf282f8d5b1e22affe549ad3492a3`, Rust authority `eb73dff36d15a829e8358d310dbb4106562b344d`. Immutable binary hashes, provider configuration, source pins/licenses and ceilings are in [preregistration](formation-exploration-100-plan.json). The first 16 cells use the original preregistration; the documented transport-only correction precedes continuation.

This is a new condition arm. All frozen source pins and existing K are preserved; the 81 baseline inputs without K receive the previously authorized explicit source-bound static K template. New Runtime/adapter code and the externally frozen exploration ceiling also differ from the historical Infer/network-denied baseline. The gain is not an isolated LLM causal estimate. Historical ledgers are unchanged.

## Funnel

| Layer | Apps |
| --- | --- |
| A_source_recognized | 99 |
| B_K_formed | 99 |
| C_D_available | 15 |
| D_admitted | 12 |
| E_executed | 12 |
| F_verifier_ran | 7 |
| G_typed_K_PASS | 7 |
| H_retained | 7 |

99 actual Coordinator search terminals plus 1 pre-search source boundary terminal. Explicitly classified untyped core source errors: 1; unexplained untyped failures: 0.

Admission is conservatively proven by an execution-started attestation, including later startup failure. Verifier ran requires actual observation verdicts or a fresh receipt; readiness failure alone does not count. Retained artifacts do not grant normal execution or deployment.

## Terminal distribution

| Primary | Apps |
| --- | --- |
| rounds_exhausted | 59 |
| no_progress | 2 |
| success | 7 |
| authority | 2 |
| dependency/network | 7 |
| runtime/adapter | 21 |
| exploration_authority_exceeded | 1 |
| source/preflight | 1 |

Primary diagnostic class and durable stop reason are separate in the JSON. An unsupported/declined proposal or unsuccessful inspection is not proof that every possible D for that application is inexpressible.

### Diagnostic observations

| Diagnostic (non-exclusive) | Apps |
| --- | --- |
| source_inspection_unauthorized | 16 |
| candidate_not_observable | 4 |
| inspection_budget_exhausted | 27 |
| source_inspection_requested | 53 |
| authority_denied | 2 |
| unsupported_dependency_manifest | 7 |
| proposal_schema | 10 |
| source_oci_builder_unavailable | 8 |
| unsupported_entrypoint | 13 |
| execution_requirements_duplicate | 1 |
| ticket_unplannable | 1 |
| execution_plan_bounds | 4 |
| exploration_authority_exceeded | 1 |
| unsupported_source_oci_selection | 2 |
| formation_failed | 2 |
| proposal_source_digest_mismatch | 1 |
| source_archive_cap_exceeded | 1 |

### Durable stop reasons

| Stop reason | Apps |
| --- | --- |
| rounds_exhausted | 89 |
| no_progress | 2 |
| submitted | 7 |
| exploration_authority_exceeded | 1 |
| source_archive_cap_exceeded | 1 |

### Proposal dispositions

| Disposition | Observations |
| --- | --- |
| rejected | 157 |
| unsupported | 97 |
| admitted | 12 |

## Actual providers and cost

| Metric | Observation |
| --- | --- |
| Apps with an actual LLM call | 92 |
| Actual LLM call rate | 92% |
| CandidateProducer calls | 273 |
| DecisionProvider calls | 1 |
| Input tokens | 2006440 |
| Output tokens | 68395 |
| Known-usage estimated USD | $0.683545 |
| Conservative frozen-reservation charge USD | $2.686616 |
| Estimated USD per additional PASS | undefined (no additional PASS) |
| Unknown-usage calls | 0 |
| Unresolved reservations | 0 |
| Consumed generation rounds | 274 |
| Known-D attempts | 7 |
| Generated-D attempts | 10 |
| Wall seconds including infrastructure recovery | 11964.62 |

CandidateProducer model identifier `deepseek-flash`, frozen prompt v3, thinking disabled and output limit 2048. The provider alias does not establish an immutable weight revision. Jev `jev-1.13.0` is configured for ambiguous hard-filtered finite choices only. Per-app no-call reasons, model/usage/latency, canonical proposals and journal hashes are retained; no credential value is in the evidence. Small-stage provider usage is separate.

## Generated D and permission recovery

| Metric | Apps |
| --- | --- |
| Valid generated D | 8 |
| Generated D admitted | 5 |
| Generated D executed | 5 |
| Generated D PASS | 0 |
| Within-search authority denial → execution | 0 |
| Within-search network denial → execution | 0 |
| Dependency failure → PASS | 0 |
| Baseline permission blocked → execution | 1 |
| Baseline permission blocked → PASS | 0 |

The [small acceptance ledger](formation-exploration-small.md) proves actual live-provider same-K repair on two runnable HTTP fixtures, and separately proves permission reduction/failed reduction, ceiling refusal, restart and UNKNOWN through the actual Coordinator/Runtime. Fixture results are not OSS coverage. The live WBO/copyparty pilot failures are preserved.

## Representative failure evidence

- WBO (2): valid phase-scoped Node/npm D; actual dependency retrieval and startup reached, then `candidate_not_observable`. A repeated canonical D ended `no_progress` without another execution. This does not borrow the cause from historical OCI file-capability refusal.
- SearXNG (6) and OpenRefine (49): canonical D existed, but declared authority was insufficient; no later D recovered to execution within this search.
- SVGOMG (57): a previously policy-blocked baseline reached generated-D execution; three generated D did not satisfy K. One real Jev finite-choice call is recorded separately from producer usage.
- Vaultwarden (41): a proposed build-phase `github.com:443` condition exceeded the frozen ceiling, ending `exploration_authority_exceeded` with no attempt. Its LLM rationale remains a proposal, not proof of necessity.
- Copyparty (69): source inspection requests consumed the frozen inspection/round budget; no canonical D or Runtime execution. The separate earlier pilot had a different result and stays separate.
- Superset (73): the selected directory-to-source preparation hit the unchanged archive cap before Search; no model or Runtime. This is not a global incompatibility claim for all possible materializations.

## Fresh typed-K successes

| Index | App | Origin | Functional evidence in this wave |
| --- | --- | --- | --- |
| 3 | Uptime Kuma | known D | not measured |
| 21 | 2048 | known D | not measured |
| 22 | reveal.js | known D | not measured |
| 52 | Hextris | known D | not measured |
| 53 | 0h h1 | known D | not measured |
| 54 | 0h n0 | known D | not measured |
| 55 | Emoji search | known D | not measured |

Uptime Kuma remains a static fallback and is not established as a monitoring service. Existing 2048/reveal.js browser acceptance remains a separate historical append; persistence is not measured.

## Language and source shape

### primary_language

| Group | Apps | Primary distribution |
| --- | --- | --- |
| Blade | 1 | rounds_exhausted=1 |
| C# | 5 | rounds_exhausted=4; runtime/adapter=1 |
| CSS | 1 | success=1 |
| Go | 13 | rounds_exhausted=9; runtime/adapter=4 |
| Java | 5 | authority=1; rounds_exhausted=3; runtime/adapter=1 |
| JavaScript | 19 | no_progress=1; success=6; runtime/adapter=3; rounds_exhausted=6; dependency/network=3 |
| JavaScript/Vue | 1 | rounds_exhausted=1 |
| PHP | 6 | rounds_exhausted=6 |
| Python | 21 | rounds_exhausted=13; authority=1; dependency/network=3; runtime/adapter=2; no_progress=1; source/preflight=1 |
| Ruby | 4 | rounds_exhausted=3; runtime/adapter=1 |
| Rust | 6 | exploration_authority_exceeded=1; runtime/adapter=3; rounds_exhausted=2 |
| TypeScript | 16 | rounds_exhausted=10; runtime/adapter=5; dependency/network=1 |
| TypeScript/JavaScript | 1 | rounds_exhausted=1 |
| Vue/TypeScript | 1 | runtime/adapter=1 |

### broad_source_shape

| Group | Apps | Primary distribution |
| --- | --- | --- |
| dotnet-service | 5 | rounds_exhausted=4; runtime/adapter=1 |
| frontend-monorepo | 1 | rounds_exhausted=1 |
| frontend-spa | 8 | rounds_exhausted=4; runtime/adapter=3; dependency/network=1 |
| go-service | 14 | rounds_exhausted=9; runtime/adapter=4; dependency/network=1 |
| jvm-service | 4 | authority=1; rounds_exhausted=2; runtime/adapter=1 |
| multi-service | 12 | rounds_exhausted=10; runtime/adapter=1; source/preflight=1 |
| node-monorepo | 5 | rounds_exhausted=4; runtime/adapter=1 |
| node-service | 10 | no_progress=1; success=1; rounds_exhausted=3; runtime/adapter=4; dependency/network=1 |
| php-service | 7 | rounds_exhausted=7 |
| plain-static | 8 | success=6; dependency/network=1; rounds_exhausted=1 |
| polyglot-service | 1 | runtime/adapter=1 |
| python-monorepo | 1 | rounds_exhausted=1 |
| python-service | 18 | rounds_exhausted=11; authority=1; dependency/network=3; runtime/adapter=2; no_progress=1 |
| ruby-service | 2 | runtime/adapter=1; rounds_exhausted=1 |
| rust-service | 4 | exploration_authority_exceeded=1; runtime/adapter=2; rounds_exhausted=1 |

## Infrastructure and evidence

The shared local owner reached the unchanged receiver source quota after 16 cells. Cell17 failed before Search creation with zero model calls/rounds/attempts. Its original checkpoint and journal were preserved. Preregistered index partitions use the same receiver code, schema, quota, owner identity, Runtime and ceilings in fresh isolated D1/R2 namespaces; all previous namespaces are retained. Only the uncreated cell17 submission was recovered with its original Search ID/source/K/policy/budget. Its opened round that expired during requester recompilation remains consumed. Completed application terminals were never rerun.

Cell60 separately exceeded the 120-second static source-capture observer wait before any Search/model/Runtime. Original failure evidence is retained. The source-only observer wait became 600 seconds; source limits, K, Runtime/search deadlines, prompt, grants and model budgets did not change. No completed application terminal was rerun. A control-log naming collision on the first continuation added zero requester deliveries/calls and was corrected with preserved separate logs.

Superset (73) hit the actual unchanged core source-object archive cap before Search/model/Runtime. Its original untyped core error is explicitly classified as source_archive_cap_exceeded by the harness, not represented as a typed Coordinator or fabricated Runtime result. No cap increase, source modification or application rerun occurred. Other completed application terminals remain untouched.

Raw evidence is retained on `oci-linux-test` under `/home/ubuntu/formation-foundation/.tmp/exploration-100-run`, with per-file hashes in [manifest](formation-exploration-100-raw-manifest.json). Frozen archives, all receiver namespaces, worker state, receipts and product journals remain retained. Wall interval `2026-09-30T12:48:59.773970+00:00` → `2026-09-30T16:08:24.394228+00:00` includes the infrastructure interruption.

[Resource and archive hashes](formation-exploration-100-resource.json) confirm all immutable binaries and receiver objects after the wave. Disk free remained 84,832,514,048 bytes; 1,205 raw files are preserved in a 24,582,590-byte archive with SHA-256 `9fba90019da0b4705202006db1676bab43ff83314837c133c4daf8ed6bf81268`. No state, evidence or frozen source was deleted.

[Task-wide budget summary](formation-exploration-budget-summary.json): small stage plus this wave estimated $0.710158, conservative charge $2.784926, remaining conservative reservation $0.722202, unresolved reservations0. These are token-based estimates and frozen ceiling accounting, not invoice receipts.

## Status

Implemented in the unmerged exploration branches; measured on the exact immutable runtime set above. Fresh receipt is verified only for submitted K/D/attempt. Submission is `k_reached_awaiting_assessment`, `approval=not_assessed`, `deployed=false`; normal verified-route authorization is not created. Post-submission assessment must bind D digest in a separate workflow.

Rust/API checks and existing CI failures are in [verification](formation-exploration-verification.json). Ubuntu CI passed; Windows unchanged Unix API failure is exact-base reproduced; macOS process activation failed in CI with an unresolved cause; local head3/base1 PASS does not establish a base-reproduced FAIL. API CI jobs did not start because of billing; local typecheck and 200 tests passed. CI is not reported green.

No deployment, remote migration, source rewrite, per-app override, permission ceiling expansion, #1421 operation or new browser acceptance occurred. Source-OCI remains a separately connected capability; the unbound builder recipe in this arm is not an automatic OCI success. Historical WBO image-build evidence and unexecuted Vikunja preregistration are unchanged.

