# Formation 6b-E — current-main 100-app measurement

Measured **100/100 actual terminals**, **7 fresh typed-K PASS**, **0 new functional acceptances**, and **0 observed regressions**.
The completion gate counts measured outcomes. It does not establish 100 usable applications or a functional success rate.

## Fixed provenance

- ato: `e494e9375cf1151fc2884d545047dfbdad52c2cc` (main observed and frozen before the wave)
- ato-api: `dcc3049a69051ed6d1c7819580ed1e171815c34c`; #1449 merge: `e494e9375cf1151fc2884d545047dfbdad52c2cc`
- Preregistration: `b9737c173cbdbb1005b8fa6fec301a599ee30ce4`, pushed before the first invocation; [100 identities and source/license hashes](formation-coverage-100-plan.json), [selection rationale](formation-coverage-100-plan.md)
- Started: `2026-09-30T04:09:10.411590+00:00`; completed: `2026-09-30T04:21:18.050733+00:00`
- Host: oci-linux-test, Ubuntu 24.04.4, Linux/aarch64, Rust/Cargo 1.96.0, local bwrap+landlock, toolchains `/opt/ato/toolchains`
- Disk available: 128,212,877,312 → 128,143,462,400 bytes; preregistered minimum 64 GiB
- coverage_baseline SHA256: `3069841405ccafd82a2d0dbba0e220f179fb89679f6efe1dd17c3577b57b9509`
- ato-formation-worker SHA256: `6de1e3a47a3974bb8b5d4e49c04bcaf9ddb96495f467d34546067bed37441739`

All 100 use the same binaries and default Runtime profile. CandidateProducer/DecisionProvider/browser model verifier are OFF, model calls 0, network denied, max attempts 4, hard timeout 900 s. No manual authoring, source-to-OCI request, app override, source rewrite or permission expansion. Original 50 source pins/licenses/archives are reused verbatim.

## Current funnel

| Layer | All 100 | Existing 50 | Additional 50 |
| --- | --- | --- | --- |
| A: source_recognized | 100 | 50 | 50 |
| B: K_formed | 19 | 8 | 11 |
| C: D_available | 19 | 8 | 11 |
| D: admitted | 7 | 3 | 4 |
| E: executed | 7 | 3 | 4 |
| F: verifier_ran | 7 | 3 | 4 |
| G: K_fully_satisfied | 7 | 3 | 4 |
| H: retained | 7 | 3 | 4 |

A requires successful source normalization; B/C require refs; D requires admission/start evidence; E/F require actual runtime/verification evidence; G requires a fresh fully_satisfied Rust receipt matching attempt/K/D; H requires a physical retained bundle and its hash inventory. I is unmeasured in this wave.

Reporting clarification: the preregistered classifier treated every failed attempt as admitted. CyberChef emits failed at projection before K/D/common-attempt start. The report counts D only when a durable start identity is present. Original raw classifier output is preserved; no app was rerun and no primary terminal or execution condition changed. The JSON records this one layer correction explicitly.

## Primary terminals

| Primary class | Apps |
| --- | --- |
| effect/policy | 12 |
| known-D/authoring | 81 |
| success | 7 |

| Typed terminal | Apps |
| --- | --- |
| fully_satisfied | 7 |
| intent_malformed | 1 |
| network_denied | 12 |
| preset_no_match | 44 |
| preset_node_static_needs_build_script | 10 |
| preset_node_static_needs_lockfile | 1 |
| preset_node_static_v2_bun_deferred | 1 |
| preset_node_static_v2_manager_config | 6 |
| preset_node_static_v2_static_unproven | 4 |
| preset_node_static_v2_workspace | 14 |

The primary class describes the observed stop. Source-based secondary hypotheses are separate and unmeasured. CandidateProducer plausibility is a separate false/unknown field, with no untested true claim. Permission and source-limit refusals cannot be repaired by generating a candidate.

## Language and source shape

| Primary language | Apps | Terminals |
| --- | --- | --- |
| Blade | 1 | known-D/authoring 1 |
| C# | 5 | known-D/authoring 5 |
| CSS | 1 | success 1 |
| Go | 13 | known-D/authoring 13 |
| Java | 5 | known-D/authoring 5 |
| JavaScript | 19 | effect/policy 2, known-D/authoring 11, success 6 |
| JavaScript/Vue | 1 | known-D/authoring 1 |
| PHP | 6 | effect/policy 3, known-D/authoring 3 |
| Python | 21 | effect/policy 2, known-D/authoring 19 |
| Ruby | 4 | effect/policy 1, known-D/authoring 3 |
| Rust | 6 | known-D/authoring 6 |
| TypeScript | 16 | effect/policy 4, known-D/authoring 12 |
| TypeScript/JavaScript | 1 | known-D/authoring 1 |
| Vue/TypeScript | 1 | known-D/authoring 1 |

Primary language is the preregistered upstream metadata label, not an inferred executor. For example, a TypeScript-dominant repository can still have a Go service build.

| Preregistered source shape | Apps | K/D | Executed | Typed-K | Terminals |
| --- | --- | --- | --- | --- | --- |
| dotnet-service | 5 | 0/0 | 0 | 0 | known-D/authoring 5 |
| frontend-monorepo | 1 | 0/0 | 0 | 0 | known-D/authoring 1 |
| frontend-spa | 8 | 3/3 | 0 | 0 | effect/policy 3, known-D/authoring 5 |
| go-service | 14 | 0/0 | 0 | 0 | known-D/authoring 14 |
| jvm-service | 4 | 0/0 | 0 | 0 | known-D/authoring 4 |
| multi-service | 12 | 1/1 | 0 | 0 | effect/policy 1, known-D/authoring 11 |
| node-monorepo | 5 | 0/0 | 0 | 0 | known-D/authoring 5 |
| node-service | 10 | 2/2 | 1 | 1 | effect/policy 1, known-D/authoring 8, success 1 |
| php-service | 7 | 3/3 | 0 | 0 | effect/policy 3, known-D/authoring 4 |
| plain-static | 8 | 6/6 | 6 | 6 | known-D/authoring 2, success 6 |
| polyglot-service | 1 | 1/1 | 0 | 0 | effect/policy 1 |
| python-monorepo | 1 | 0/0 | 0 | 0 | known-D/authoring 1 |
| python-service | 18 | 2/2 | 0 | 0 | effect/policy 2, known-D/authoring 16 |
| ruby-service | 2 | 1/1 | 0 | 0 | effect/policy 1, known-D/authoring 1 |
| rust-service | 4 | 0/0 | 0 | 0 | known-D/authoring 4 |

These source hypotheses and proportions were fixed before measurement. Source-only license replacements are recorded in the plan; no post-result substitutions.

## Existing 50 comparison

Historical pin `c4c285f3e9a751975286ccd78ba49e76e483fa83` → this wave `e494e9375cf1151fc2884d545047dfbdad52c2cc`. Historical ledgers are byte-unchanged. [Per-app before/after](formation-coverage-50-current-comparison.md).

| Change | Apps |
| --- | --- |
| reach_improvement | 1 |
| unchanged | 49 |

Progress is compared per app, not by adding old 50 results to new 50. Ref-only/diagnostic changes do not imply a functional improvement. No application failure was rerun.

## Fresh typed-K successes

| Index | App | Preregistered hypothesis | Later functional candidate |
| --- | --- | --- | --- |
| 3 | Uptime Kuma | service | no |
| 21 | 2048 | static | yes |
| 22 | reveal.js | static | yes |
| 52 | Hextris | static | yes |
| 53 | 0h h1 | static | yes |
| 54 | 0h n0 | static | yes |
| 55 | Emoji search | static | yes |

**Uptime Kuma is a static fallback satisfying a weak static K; monitoring service functionality is not established and it is not counted as a functioning service.** All seven executed routes are static. The four new typed-K apps require source-matched functional acceptance before claiming actual application usability.

2048/reveal.js retain separate historical Chrome UI evidence for two unique apps at the c4c285f3 binary pin; [functional append](formation-static-functional-append-2026-09-30.md). This wave does not repeat browser operations, establish persistence, or turn that evidence into a 2/100 functional rate. GET / = 200 is not functional acceptance.

## Generic capability history and remaining work

#1446/#1448 provide generic explicit source-to-OCI isolation/materialization. They were not invoked by this automatic baseline. WBO image build and verified OCI artifact PASS remain separate; start stops at runtime_policy_capability_required, C/D/E unreached, functional 0. Vikunja remains preregistered and unexecuted; its prerequisite blocker is not a build failure. [Historical append](formation-coverage-50-source-oci-append.json).

Regressions to fix: 0. No capabilities or regressions were fixed in this measurement PR. The measured distribution informs whether to proceed to 6c or separately address one generic blocker. No implementation choice is made here.

## Evidence and status

[Machine ledger](formation-coverage-100.json), [raw manifest](evidence/formation-coverage-100-20260930/raw-manifest.json), [raw evidence archive](evidence/formation-coverage-100-20260930/raw-evidence.tar.gz). The archive preserves stdout/stderr, each typed result, attempt journals, fresh receipts, retained bundle manifests/inventories, start times, process results, build log and source-identity check. Full frozen sources, bundles and state remain on the measurement host.

Raw results SHA256: `af90c583c64529aa1909d9ce7135643ea6d3236f543a9a1720e997f805de2fe9`. Evidence archive SHA256: `890ec59520ccb37ce789b8e5d5c0fa5c9e28c406b52a4a1daa5d6521608a330a`.

No infra failures, untyped failures, hard timeouts or reruns. All 100 terminals are classified. No host-wide prune or state/evidence deletion.

CI is not assumed green. At exact base `e494e937`, Windows Rust compilation fails on Unix-only browser_sandbox APIs and Ubuntu fails `hosted_python_and_node_use_the_common_process_runtime` at `runtime_cannot_contain_build`. [Base CI snapshot and log hashes](evidence/formation-coverage-100-20260930/ci-base.json) and [logs](evidence/formation-coverage-100-20260930/ci-base-evidence.tar.gz) preserve that evidence. macOS was still running in the snapshot. Head checks require live PR inspection; no unobserved head failure is called base-reproduced. Rust/Cargo/workflow code is unchanged in this PR.

- Implemented: no new Formation capability; preregistered measurement/reporting scripts only.
- Measured: 100 upstream applications, one current code/policy/runtime condition.
- Verified: 7 fresh typed-K receipts; 0 functional tests in this wave.
- Merged: runtime pin and #1449 already merged; this measurement evidence awaits PR review.
- Deployed: none; no remote migration, no model credential reread, reservation 4,513,388 USD micros unused, #1421 untouched.
