# 2026 archive

These are dated reports, past plans, experiments and retired procedures.
They preserve evidence; they do not certify current deployment or amend an
accepted specification. English is the default for current documentation.
Japanese/mixed source documents are labelled below and retain their original text.

## Retained collections

| Collection | Classification / location |
|---|---|
| [Dev consolidation](../../ops/dev-consolidation-20261026/VALIDATION.md) | Archive; Japanese/mixed reports and unchanged raw JSON |
| [Ready-State](../../ready-state/README.md) | Archive; experiments, benchmarks and past host contracts |
| [Internal plans/research/handoffs/dashboards](../../internal/README.md) | Archive; test-isolation guide remains current separately |
| [Dev notes](../../dev-notes/) | Archive; past Desktop/installation audits |
| [Receipts](../../receipts/) | Archive; dated implementation acceptance evidence |
| [AODD](../../aodd/) | Archive; historical acceptance and community-publish records |
| [Recipe evaluations](../../recipes/) | Archive for dated evaluations/plans; bounded behavior notes retain their individual status |
| [Raw ops evidence](../../ops/README.md) | Archive; original JSON/log/image/archive paths preserved |

## Moved documents

The table lists every archived move. Former paths retain compatibility pages
and heading anchors. [moves.tsv](documentation-reorganization/moves.tsv) also
lists current runbooks and architecture moves. The
[complete inventory](documentation-reorganization/inventory.tsv) covers retained
files and classifies each into Concepts, Specifications, Guides, Operations or
Archive, including draft status and baseline checksums.

| Preserved document | Language | Former path |
|---|---|---|
| [Computation architecture migration matrix](legacy/MIGRATION_MATRIX.md) | en | `docs/MIGRATION_MATRIX.md` |
| [Capsule v0.3 to v1 migration](legacy/capsule-v1-migration.md) | en | `docs/capsule-v1-migration.md` |
| [Always-on and TCP network controls staging acceptance — 2026-09-19](ops/always-on-network-controls-staging-2026-09-19.md) | en | `docs/ops/always-on-network-controls-staging-2026-09-19.md` |
| [B1 Formation Service v1 — acceptance closeout](ops/b1-formation-acceptance.md) | en | `docs/ops/b1-formation-acceptance.md` |
| [B1 Formation — staging acceptance record](ops/b1-formation-staging-acceptance.md) | en | `docs/ops/b1-formation-staging-acceptance.md` |
| [B1_BASELINE — consolidated nightly](ops/b1-nightly-baseline.md) | en | `docs/ops/b1-nightly-baseline.md` |
| [Bounded source acquisition verification — 2026-10-24](ops/bounded-github-source-fetch-20261024.md) | en | `docs/ops/bounded-github-source-fetch-20261024.md` |
| [Browser Verifier containment — acceptance (2026-09-23)](ops/browser-verifier-containment-acceptance-2026-09-23.md) | en | `docs/ops/browser-verifier-containment-acceptance-2026-09-23.md` |
| [Component Contract observation implementation — 2026-10-24](ops/component-contract-observation-20261024.md) | en | `docs/ops/component-contract-observation-20261024.md` |
| [Connected Runner concurrency: `--max-slots` / `ATO_RUNNER_MAX_SLOTS`](ops/connected-runner-max-slots.md) | en | `docs/ops/connected-runner-max-slots.md` |
| [Connected Runner native-inference smoke (#754 gate)](ops/connected-runner-native-inference-smoke.md) | en | `docs/ops/connected-runner-native-inference-smoke.md` |
| [Continuous patching ledger](ops/continuous-patching-ledger.md) | en | `docs/ops/continuous-patching-ledger.md` |
| [Contract-preserving Derivation transformation v0 — E2E evidence](ops/contract-preserving-transformation-v0.md) | ja+en | `docs/ops/contract-preserving-transformation-v0.md` |
| [CVE-2026-111497（Node.js 同梱 Brotli）対応: Node 版カタログの更新](ops/cve-2026-111497-node-brotli-20261024.md) | ja+en | `docs/ops/cve-2026-111497-node-brotli-20261024.md` |
| [Explicit runtime placement — 2026-10-24](ops/explicit-runtime-placement-20261024.md) | en | `docs/ops/explicit-runtime-placement-20261024.md` |
| [Formation 2e-b — Hosted LocalProcess ownership](ops/formation-2e-b-ownership-2026-09-25.md) | en | `docs/ops/formation-2e-b-ownership-2026-09-25.md` |
| [Formation 2e-c — OCI container and service-group ownership](ops/formation-2e-c-oci-ownership-2026-09-25.md) | en | `docs/ops/formation-2e-c-oci-ownership-2026-09-25.md` |
| [Formation 2f — implementation starting point](ops/formation-2f-ir-reduction-2026-09-25.md) | en | `docs/ops/formation-2f-ir-reduction-2026-09-25.md` |
| [Stage 3a receipt authority — local evidence, 2026-09-25](ops/formation-3a-acceptance-2026-09-25.md) | en | `docs/ops/formation-3a-acceptance-2026-09-25.md` |
| [Formation 5a-a — DecisionProvider verification ledger (2026-09-26)](ops/formation-5a-decision-provider-2026-09-26.md) | en | `docs/ops/formation-5a-decision-provider-2026-09-26.md` |
| [Formation 5a-b — finite exploration actions verification ledger (2026-09-26)](ops/formation-5b-exploration-actions-2026-09-26.md) | en | `docs/ops/formation-5b-exploration-actions-2026-09-26.md` |
| [Formation 6b-F — preregistered adaptive arm](ops/formation-adaptive-100-plan.md) | en | `docs/ops/formation-adaptive-100-plan.md` |
| [Formation 6b-F — 100-app Adaptive arm](ops/formation-adaptive-100.md) | ja+en | `docs/ops/formation-adaptive-100.md` |
| [Formation 5c — implementation and actual merge blocker](ops/formation-adaptive-5c-pre0307.md) | en | `docs/ops/formation-adaptive-5c-pre0307.md` |
| [Formation 5c — bounded adaptive integration](ops/formation-adaptive-5c.md) | en | `docs/ops/formation-adaptive-5c.md` |
| [Formation Agent Session受入 — 2026-10-04](ops/formation-agent-acceptance-current-20261004.md) | ja+en | `docs/ops/formation-agent-acceptance-current-20261004.md` |
| [Formation agent CI comparison — 2026-10-03](ops/formation-agent-ci-comparison-20261003.md) | en | `docs/ops/formation-agent-ci-comparison-20261003.md` |
| [Formation v0共通Session・Skillの引き継ぎ](ops/formation-agent-delivery-20261003.md) | ja+en | `docs/ops/formation-agent-delivery-20261003.md` |
| [Formation共通Session・状態経路の統合検証](ops/formation-agent-integration-local-20261003.md) | ja+en | `docs/ops/formation-agent-integration-local-20261003.md` |
| [Windows修正を含む統合Sourceの追加検証](ops/formation-agent-integration-windows-fix-local-20261003.md) | ja+en | `docs/ops/formation-agent-integration-windows-fix-local-20261003.md` |
| [Formation Session変更に伴うnative Linux CI比較](ops/formation-agent-linux-ci-comparison-20261003.md) | ja+en | `docs/ops/formation-agent-linux-ci-comparison-20261003.md` |
| [Formation agent Windows-target follow-up — 2026-10-03](ops/formation-agent-windows-ci-final-20261003.md) | en | `docs/ops/formation-agent-windows-ci-final-20261003.md` |
| [Verified raw archive to existing Search — 2026-10-24](ops/formation-approved-source-search-20261024.md) | en | `docs/ops/formation-approved-source-search-20261024.md` |
| [Formation Codex-first探索の実装と受入記録 — 2026-10-01](ops/formation-autonomous-reasoning-20261001.md) | ja+en | `docs/ops/formation-autonomous-reasoning-20261001.md` |
| [Source-backed private HTTP Port acceptance](ops/formation-bound-http-port-acceptance-20261002.md) | en | `docs/ops/formation-bound-http-port-acceptance-20261002.md` |
| [Formation Browser Verification v0 — acceptance (2026-09-23)](ops/formation-browser-verification-v0-acceptance-2026-09-23.md) | en | `docs/ops/formation-browser-verification-v0-acceptance-2026-09-23.md` |
| [Formation build controller environment boundary](ops/formation-build-controller-environment-20261024.md) | en | `docs/ops/formation-build-controller-environment-20261024.md` |
| [Native build evidence retention before runnable workspace projection](ops/formation-build-record-retention-20261002.md) | en | `docs/ops/formation-build-record-retention-20261002.md` |
| [Formation #1449 — final reviewed head CI recheck](ops/formation-ci-static-recheck-2026-09-30.md) | en | `docs/ops/formation-ci-static-recheck-2026-09-30.md` |
| [Formation common attempt (ato#1390) — Runtime Network acceptance (2026-09-24)](ops/formation-common-attempt-acceptance-2026-09-24.md) | en | `docs/ops/formation-common-attempt-acceptance-2026-09-24.md` |
| [Formation 5b-c1: context/2 provider integration, offline verification](ops/formation-context-provider-c1-2026-09-28.md) | en | `docs/ops/formation-context-provider-c1-2026-09-28.md` |
| [c1 review hardening: claim boundary](ops/formation-context-provider-c1-hardening-2026-09-28.md) | en | `docs/ops/formation-context-provider-c1-hardening-2026-09-28.md` |
| [Formation 5b-c0 — offline bounded information recovery](ops/formation-context-recovery-c0-2026-09-27.md) | en | `docs/ops/formation-context-recovery-c0-2026-09-27.md` |
| [c0 review corrections before c1](ops/formation-context-recovery-c0-review-2026-09-28.md) | en | `docs/ops/formation-context-recovery-c0-review-2026-09-28.md` |
| [Coordinator-backed Formation state verification](ops/formation-coordinator-state-verification-20261002.md) | en | `docs/ops/formation-coordinator-state-verification-20261002.md` |
| [Formation 6b-E — current-main 100-app preregistration](ops/formation-coverage-100-plan.md) | en | `docs/ops/formation-coverage-100-plan.md` |
| [Formation 6b-E — current-main 100-app measurement](ops/formation-coverage-100.md) | en | `docs/ops/formation-coverage-100.md` |
| [Formation 6a — 20-app known-D baseline preregistration](ops/formation-coverage-20-plan.md) | en | `docs/ops/formation-coverage-20-plan.md` |
| [Formation 6a — 20-app known-D baseline results](ops/formation-coverage-20.md) | en | `docs/ops/formation-coverage-20.md` |
| [Formation historical 50 → current 50](ops/formation-coverage-50-current-comparison.md) | en | `docs/ops/formation-coverage-50-current-comparison.md` |
| [Formation 6b-C — 20 → 50 coverage preregistration](ops/formation-coverage-50-plan.md) | en | `docs/ops/formation-coverage-50-plan.md` |
| [6b-C appendix — npm v1 routing correction (LibreChat)](ops/formation-coverage-50-routing-correction.md) | en | `docs/ops/formation-coverage-50-routing-correction.md` |
| [Formation 6b-C — 20 → 50 actual coverage measurement](ops/formation-coverage-50.md) | en | `docs/ops/formation-coverage-50.md` |
| [Formation current support — 2026-09-30](ops/formation-current-support-2026-09-30.md) | en | `docs/ops/formation-current-support-2026-09-30.md` |
| [Formation D2-C acceptance — 2026-09-28](ops/formation-deepseek-d2-2026-09-28.md) | en | `docs/ops/formation-deepseek-d2-2026-09-28.md` |
| [D3-E execution controller — offline review, live BLOCKED](ops/formation-deepseek-d3-execution-driver-review.md) | en | `docs/ops/formation-deepseek-d3-execution-driver-review.md` |
| [D3-E v2 execution driver review](ops/formation-deepseek-d3-execution-driver-v2-review.md) | en | `docs/ops/formation-deepseek-d3-execution-driver-v2-review.md` |
| [Formation D3 v2 — exact request evidence preregistration](ops/formation-deepseek-d3-plan-v2.md) | en | `docs/ops/formation-deepseek-d3-plan-v2.md` |
| [Formation DeepSeek D3 — preregistration (NOT EXECUTED)](ops/formation-deepseek-d3-plan.md) | en | `docs/ops/formation-deepseek-d3-plan.md` |
| [D3 v2 live acceptance — 5b completion](ops/formation-deepseek-d3-results.md) | en | `docs/ops/formation-deepseek-d3-results.md` |
| [D3 v2 request evidence — verification, no live execution](ops/formation-deepseek-d3-v2-verification.md) | en | `docs/ops/formation-deepseek-d3-v2-verification.md` |
| [E1 protocol amendment — comparator integration error](ops/formation-efficacy-e1-amendment.md) | en | `docs/ops/formation-efficacy-e1-amendment.md` |
| [E1 aggregation completeness correction](ops/formation-efficacy-e1-completeness-review.md) | en | `docs/ops/formation-efficacy-e1-completeness-review.md` |
| [Post-E1 fixture-label audit (not a fourth efficacy arm)](ops/formation-efficacy-e1-oracle-plan.md) | en | `docs/ops/formation-efficacy-e1-oracle-plan.md` |
| [E1 preregistration — bounded Python generation efficacy](ops/formation-efficacy-e1-plan.md) | en | `docs/ops/formation-efficacy-e1-plan.md` |
| [Formation E1 efficacy results — 2026-09-27](ops/formation-efficacy-e1-results.md) | en | `docs/ops/formation-efficacy-e1-results.md` |
| [E2登録と現行mainの統合境界](ops/formation-efficacy-e2-merge-20261001.md) | ja+en | `docs/ops/formation-efficacy-e2-merge-20261001.md` |
| [E2 preregistration — prospective holdout efficacy evaluation](ops/formation-efficacy-e2-plan.md) | en | `docs/ops/formation-efficacy-e2-plan.md` |
| [Formation artifact embedding failure evidence, 2026-10-02](ops/formation-embedding-failure-evidence-20261002.md) | en | `docs/ops/formation-embedding-failure-evidence-20261002.md` |
| [Expired Search reporting — 2026-10-02](ops/formation-expired-search-report-20261002.md) | en | `docs/ops/formation-expired-search-report-20261002.md` |
| [Bounded autonomous Formation exploration — preregistration](ops/formation-exploration-100-plan.md) | en | `docs/ops/formation-exploration-100-plan.md` |
| [Formation exploration: frozen 100 OSS arm](ops/formation-exploration-100.md) | en | `docs/ops/formation-exploration-100.md` |
| [Exploration D-conversion diagnosis](ops/formation-exploration-diagnostics.md) | en | `docs/ops/formation-exploration-diagnostics.md` |
| [Final placement/validation repair acceptance preregistration](ops/formation-exploration-final-repair-plan.md) | en | `docs/ops/formation-exploration-final-repair-plan.md` |
| [Non-execution metadata normalization acceptance preregistration](ops/formation-exploration-metadata-repair-plan.md) | en | `docs/ops/formation-exploration-metadata-repair-plan.md` |
| [Preregistered refusal-feedback repair pilot](ops/formation-exploration-refusal-repair-plan.md) | en | `docs/ops/formation-exploration-refusal-repair-plan.md` |
| [Preregistered D-conversion repair pilot](ops/formation-exploration-repair-pilot-plan.md) | en | `docs/ops/formation-exploration-repair-pilot-plan.md` |
| [Exploration D-conversion repair: actual OSS append](ops/formation-exploration-repair.md) | en | `docs/ops/formation-exploration-repair.md` |
| [Autonomous exploration small gate](ops/formation-exploration-small-plan.md) | en | `docs/ops/formation-exploration-small-plan.md` |
| [Bounded exploration: actual provider and Runtime acceptance](ops/formation-exploration-small.md) | en | `docs/ops/formation-exploration-small.md` |
| [Formation final Source and functional regression](ops/formation-final-source-regression-20261002.md) | en | `docs/ops/formation-final-source-regression-20261002.md` |
| [Formation foundation track — working verification ledger](ops/formation-foundation-track-2026-09-25.md) | en | `docs/ops/formation-foundation-track-2026-09-25.md` |
| [Formation汎用化 調査台帳](ops/formation-generalization-inventory-20261023.md) | ja+en | `docs/ops/formation-generalization-inventory-20261023.md` |
| [Formation 5b-b — bounded source evidence acceptance](ops/formation-generation-context-2026-09-27.md) | en | `docs/ops/formation-generation-context-2026-09-27.md` |
| [Formation 5b stack review — 2026-09-27](ops/formation-generation-stack-review-2026-09-27.md) | en | `docs/ops/formation-generation-stack-review-2026-09-27.md` |
| [Formation sequential integration — 2026-09-25](ops/formation-integrated-2026-09-25.md) | en | `docs/ops/formation-integrated-2026-09-25.md` |
| [Formation known-D vs adaptive — paired100](ops/formation-known-vs-adaptive-100.md) | ja+en | `docs/ops/formation-known-vs-adaptive-100.md` |
| [Known-D baseline versus bounded exploration](ops/formation-known-vs-exploration-100.md) | en | `docs/ops/formation-known-vs-exploration-100.md` |
| [保存済みKutt Source Searchからの製品経路受入](ops/formation-kutt-source-result-product-20261005.md) | ja+en | `docs/ops/formation-kutt-source-result-product-20261005.md` |
| [Formation 5a live Jev completion gate — 2026-09-26](ops/formation-live-jev-2026-09-26.md) | en | `docs/ops/formation-live-jev-2026-09-26.md` |
| [Local Formation Phase 1 — Linux acceptance (2026-09-23)](ops/formation-local-phase1-linux-acceptance-2026-09-23.md) | en | `docs/ops/formation-local-phase1-linux-acceptance-2026-09-23.md` |
| [Local Formation source capture exclusions](ops/formation-local-source-capture-policy-20261024.md) | en | `docs/ops/formation-local-source-capture-policy-20261024.md` |
| [Formation統合SourceとCargo targetの照合](ops/formation-native-integration-cache-20261003.md) | ja+en | `docs/ops/formation-native-integration-cache-20261003.md` |
| [Native受入の開始境界ガード](ops/formation-native-start-admission-20261004.md) | ja+en | `docs/ops/formation-native-start-admission-20261004.md` |
| [Isolated Python native toolchain claims, 2026-10-02](ops/formation-native-toolchain-claims-20261002.md) | en | `docs/ops/formation-native-toolchain-claims-20261002.md` |
| [実UNKNOWN専用の追加受入](ops/formation-native-unknown-supplemental-20261005.md) | ja+en | `docs/ops/formation-native-unknown-supplemental-20261005.md` |
| [Formation 6b-A — canonical Node build route and eight-app remeasurement](ops/formation-node-build-8.md) | en | `docs/ops/formation-node-build-8.md` |
| [6b-A qualification: version-pinned package-manager-aware workspace build](ops/formation-node-build-qualification.md) | en | `docs/ops/formation-node-build-qualification.md` |
| [Formation 6b-B — workspace-scoped static build authoring](ops/formation-node-static-workspace.md) | en | `docs/ops/formation-node-static-workspace.md` |
| [OCI candidate comparison — three existing source pins](ops/formation-oci-candidates-2026-09-30.md) | en | `docs/ops/formation-oci-candidates-2026-09-30.md` |
| [Formation 6b-D1 — native process vs Dockerfile→OCI qualification](ops/formation-oci-source-qualification-d1.md) | en | `docs/ops/formation-oci-source-qualification-d1.md` |
| [P0 20 Apps Formation Benchmark — baseline (2026-09-23)](ops/formation-p0-benchmark-2026-09-23.md) | en | `docs/ops/formation-p0-benchmark-2026-09-23.md` |
| [Formation P0 remeasure on the foundation stack — 2026-09-26](ops/formation-p0-remeasure-2026-09-26.md) | en | `docs/ops/formation-p0-remeasure-2026-09-26.md` |
| [Formation P0 unblock 1 — acceptance (2026-09-23)](ops/formation-p0-unblock-1-2026-09-23.md) | en | `docs/ops/formation-p0-unblock-1-2026-09-23.md` |
| [Formation P0 unblock 2 — acceptance (2026-09-23)](ops/formation-p0-unblock-2-2026-09-23.md) | en | `docs/ops/formation-p0-unblock-2-2026-09-23.md` |
| [Formation P0 unblock 3 — authored `exec` projection (2026-09-23)](ops/formation-p0-unblock-3-2026-09-23.md) | en | `docs/ops/formation-p0-unblock-3-2026-09-23.md` |
| [Formation P0 unblock 4 — generic Process + Node / npm / pnpm / yarn (2026-09-24)](ops/formation-p0-unblock-4-2026-09-24.md) | en | `docs/ops/formation-p0-unblock-4-2026-09-24.md` |
| [CI依存を含むreview unitのnative確認](ops/formation-post-ci-dependency-20261003.md) | ja+en | `docs/ops/formation-post-ci-dependency-20261003.md` |
| [Formation PR統合レビュー — 2026-10-01](ops/formation-pr-merge-review-20261001.md) | ja+en | `docs/ops/formation-pr-merge-review-20261001.md` |
| [Process artifact transport — 2026-10-02](ops/formation-process-artifact-transport-20261002.md) | en | `docs/ops/formation-process-artifact-transport-20261002.md` |
| [Formation CandidateProducer — core semantics correction](ops/formation-producer-core-correction-2026-09-28.md) | en | `docs/ops/formation-producer-core-correction-2026-09-28.md` |
| [Formation Producer隔離preflight](ops/formation-producer-isolation-preflight-20261003.md) | ja+en | `docs/ops/formation-producer-isolation-preflight-20261003.md` |
| [Formation Producer network profile: failed OS gates and fail-closed preflight](ops/formation-producer-network-profile-20261003.md) | ja+en | `docs/ops/formation-producer-network-profile-20261003.md` |
| [CandidateProducer C2 — fixed requester actual acceptance](ops/formation-proposal-requester-2026-09-28.md) | en | `docs/ops/formation-proposal-requester-2026-09-28.md` |
| [Formation provider split — partial implementation checkpoint](ops/formation-provider-split-2026-09-28.md) | en | `docs/ops/formation-provider-split-2026-09-28.md` |
| [Python artifact retention — 2026-10-02](ops/formation-python-artifact-retention-20261002.md) | en | `docs/ops/formation-python-artifact-retention-20261002.md` |
| [Runtime readiness deadline classification — 2026-10-02](ops/formation-readiness-deadline-classification-20261002.md) | en | `docs/ops/formation-readiness-deadline-classification-20261002.md` |
| [Repository evidence in automatic Formation — 2026-10-24](ops/formation-repository-automation-20261024.md) | en | `docs/ops/formation-repository-automation-20261024.md` |
| [Formation retained state — 分離したレビュー単位の検証](ops/formation-retained-state-review-local-20261003.md) | ja+en | `docs/ops/formation-retained-state-review-local-20261003.md` |
| [Source-owned Runtime setup deadline regression — 2026-10-02](ops/formation-runtime-script-deadline-20261002.md) | en | `docs/ops/formation-runtime-script-deadline-20261002.md` |
| [Formation 6b-D0 — service/process source qualification (41 apps)](ops/formation-service-qualification-d0.md) | en | `docs/ops/formation-service-qualification-d0.md` |
| [Formation Session Producer relay: 2026-10-03](ops/formation-session-producer-relay-20261003.md) | ja+en | `docs/ops/formation-session-producer-relay-20261003.md` |
| [Formation共通Sessionのローカル検証](ops/formation-session-protocol-local-20261003.md) | ja+en | `docs/ops/formation-session-protocol-local-20261003.md` |
| [Formation Session / Native Skill follow-up — 2026-10-04](ops/formation-session-unix-native-20261004.md) | ja+en | `docs/ops/formation-session-unix-native-20261004.md` |
| [Formation: Codex-first shared reasoning prototype](ops/formation-shared-reasoning.md) | ja+en | `docs/ops/formation-shared-reasoning.md` |
| [共通Skillの最終ローカル検証](ops/formation-skill-integration-local-20261003.md) | ja+en | `docs/ops/formation-skill-integration-local-20261003.md` |
| [Formation source measurement evidence — 2026-10-24](ops/formation-source-measurements-20261024.md) | en | `docs/ops/formation-source-measurements-20261024.md` |
| [Formation 6b-D2 hardening — pinned base graph, release on every path, private session](ops/formation-source-oci-d2-hardening.md) | en | `docs/ops/formation-source-oci-d2-hardening.md` |
| [Formation 6b-D2 — source → OCI materialization (offline integration)](ops/formation-source-oci-d2.md) | en | `docs/ops/formation-source-oci-d2.md` |
| [Source-only archive storage implementation](ops/formation-source-only-storage-20261024.md) | en | `docs/ops/formation-source-only-storage-20261024.md` |
| [Source Resultからの明示的な機能検証](ops/formation-source-result-owner-entry-20261004.md) | ja+en | `docs/ops/formation-source-result-owner-entry-20261004.md` |
| [Private functional acceptance with current Formation input grants](ops/formation-state-verification-input-grants-20261002.md) | en | `docs/ops/formation-state-verification-input-grants-20261002.md` |
| [Static functional acceptance append — 2048 and reveal.js](ops/formation-static-functional-append-2026-09-30.md) | en | `docs/ops/formation-static-functional-append-2026-09-30.md` |
| [Durable retry limits for Requester status reads](ops/formation-status-transport-retries-20261002.md) | en | `docs/ops/formation-status-transport-retries-20261002.md` |
| [Formation 5b-a — typed generation, including an unmet live target](ops/formation-typed-generation-2026-09-26.md) | en | `docs/ops/formation-typed-generation-2026-09-26.md` |
| [Candidate stop preservation — 2026-10-02](ops/formation-unconfirmed-stop-preservation-20261002.md) | en | `docs/ops/formation-unconfirmed-stop-preservation-20261002.md` |
| [Formation v0 exploration control — 2026-10-01](ops/formation-v0-control-20261001.md) | en | `docs/ops/formation-v0-control-20261001.md` |
| [Formation v0 owner input — 2026-10-01](ops/formation-v0-input-20261001.md) | en | `docs/ops/formation-v0-input-20261001.md` |
| [Formation v0 native dependency operations — 2026-10-01](ops/formation-v0-native-20261001.md) | ja+en | `docs/ops/formation-v0-native-20261001.md` |
| [Formation v0 OCI proposal integration — 2026-10-01](ops/formation-v0-oci-20261001.md) | en | `docs/ops/formation-v0-oci-20261001.md` |
| [Formation v0 release gate progress — 2026-10-06 JST](ops/formation-v0-release-progress-20261006.md) | en | `docs/ops/formation-v0-release-progress-20261006.md` |
| [Formation v0 small acceptance — 2026-10-02](ops/formation-v0-small-acceptance-20261002.md) | en | `docs/ops/formation-v0-small-acceptance-20261002.md` |
| [Explicit verification state binding — 2026-10-02](ops/formation-verification-state-bindings-20261002.md) | en | `docs/ops/formation-verification-state-bindings-20261002.md` |
| [Formation — WBO isolated online build (6b-D2 follow-up)](ops/formation-wbo-online.md) | en | `docs/ops/formation-wbo-online.md` |
| [Wheel metadata header boundary — 2026-10-02](ops/formation-wheel-metadata-headers-20261002.md) | en | `docs/ops/formation-wheel-metadata-headers-20261002.md` |
| [Windows attempt journalの限定修正](ops/formation-windows-attempt-journal-local-20261003.md) | ja+en | `docs/ops/formation-windows-attempt-journal-local-20261003.md` |
| [Windows attempt journal修正後のnative CI](ops/formation-windows-attempt-journal-native-20261003.md) | ja+en | `docs/ops/formation-windows-attempt-journal-native-20261003.md` |
| [Gmail API Support Desk acceptance — 2026-09-20](ops/gmail-support-desk-2026-09-20.md) | en | `docs/ops/gmail-support-desk-2026-09-20.md` |
| [Hosted common attempt: stage 2d acceptance](ops/hosted-formation-2d-acceptance-2026-09-25.md) | en | `docs/ops/hosted-formation-2d-acceptance-2026-09-25.md` |
| [統合した通常Runnerの導入候補](ops/integrated-runner-candidate-20261024.md) | ja+en | `docs/ops/integrated-runner-candidate-20261024.md` |
| [Managed embedding claimed-lease CPU fixture](ops/managed-embedding-lease-fixture-20261024.md) | en | `docs/ops/managed-embedding-lease-fixture-20261024.md` |
| [CPU managed embedding transport acceptance — 2026-10-24](ops/managed-embedding-relay-20261024.md) | en | `docs/ops/managed-embedding-relay-20261024.md` |
| [Native application observation Adapter — 2026-10-24](ops/native-application-probe-20261024.md) | en | `docs/ops/native-application-probe-20261024.md` |
| [Native GUI / browser 結合の統合証跡](ops/native-browser-integration-20261024.md) | ja+en | `docs/ops/native-browser-integration-20261024.md` |
| [Native browser joint fixture — 2026-10-24](ops/native-browser-joint-fixture-20261024.md) | en | `docs/ops/native-browser-joint-fixture-20261024.md` |
| [Native controls / Source / Model / Group integration](ops/native-controls-integration-20261024.md) | ja+en | `docs/ops/native-controls-integration-20261024.md` |
| [Native framebuffer resize boundary](ops/native-framebuffer-resize-boundary-20261024.md) | en | `docs/ops/native-framebuffer-resize-boundary-20261024.md` |
| [Trusted Native helper controls — 2026-10-24](ops/native-helper-controls-20261024.md) | en | `docs/ops/native-helper-controls-20261024.md` |
| [Native-inference GPU (Vulkan) — Ubuntu NVIDIA runbook](ops/native-inference-vulkan-gpu-runbook.md) | en | `docs/ops/native-inference-vulkan-gpu-runbook.md` |
| [Combined Native / ModelSet / OCI component regression — 2026-10-24](ops/native-model-component-integration-20261024.md) | en | `docs/ops/native-model-component-integration-20261024.md` |
| [Runner-owned isolated GUI producer — 2026-10-24](ops/native-owned-pixel-20261024.md) | en | `docs/ops/native-owned-pixel-20261024.md` |
| [Owned Native Terminal execution — 2026-10-24](ops/native-owned-terminal-20261024.md) | en | `docs/ops/native-owned-terminal-20261024.md` |
| [Native Pixel + Instance files under a root HostBoundary Runner](ops/native-pixel-host-boundary-files-20261024.md) | en | `docs/ops/native-pixel-host-boundary-files-20261024.md` |
| [Default-off owned Pixel lease dispatch — 2026-10-24](ops/native-pixel-lease-dispatch-20261024.md) | en | `docs/ops/native-pixel-lease-dispatch-20261024.md` |
| [Portable native format admission — 2026-10-24](ops/native-portable-admission-20261024.md) | en | `docs/ops/native-portable-admission-20261024.md` |
| [Bounded RFB noVNC encoding compatibility](ops/native-rfb-encoding-compat-20261024.md) | en | `docs/ops/native-rfb-encoding-compat-20261024.md` |
| [Native Surface execution scope — 2026-10-24](ops/native-surface-execution-scope-20261024.md) | en | `docs/ops/native-surface-execution-scope-20261024.md` |
| [Native Surface Linux producer measurement — 2026-10-23 UTC](ops/native-surface-linux-20261023.md) | en | `docs/ops/native-surface-linux-20261023.md` |
| [Native transport acceptance — 2026-10-24](ops/native-surface-transport-20261024.md) | en | `docs/ops/native-surface-transport-20261024.md` |
| [Owned Terminal browser joint acceptance — 2026-10-24](ops/native-terminal-browser-joint-20261024.md) | en | `docs/ops/native-terminal-browser-joint-20261024.md` |
| [Native Terminal connected lease dispatch — 2026-10-24](ops/native-terminal-lease-dispatch-20261024.md) | en | `docs/ops/native-terminal-lease-dispatch-20261024.md` |
| [Private Terminal producer — 2026-10-24](ops/native-terminal-unix-producer-20261024.md) | en | `docs/ops/native-terminal-unix-producer-20261024.md` |
| [Native text paste boundary — 2026-10-24](ops/native-text-paste-boundary-20261024.md) | en | `docs/ops/native-text-paste-boundary-20261024.md` |
| [OCI component input placement — 2026-10-24](ops/oci-component-inputs-20261024.md) | en | `docs/ops/oci-component-inputs-20261024.md` |
| [Run file delivery into single-container OCI](ops/oci-instance-files-20261024.md) | en | `docs/ops/oci-instance-files-20261024.md` |
| [OCI profile: `oci.working_dir`, state over an image VOLUME, no anonymous volumes](ops/oci-profile-working-dir-state.md) | en | `docs/ops/oci-profile-working-dir-state.md` |
| [Writable OCI state admission (#790 / API #783)](ops/oss-790-nonroot-state-2026-10-15.md) | en | `docs/ops/oss-790-nonroot-state-2026-10-15.md` |
| [P3 Dynamic Compute — staging acceptance record](ops/p3-dynamic-compute-staging-acceptance.md) | en | `docs/ops/p3-dynamic-compute-staging-acceptance.md` |
| [P4 — Stable Instance Host, automatic wake/sleep, and Schema Update](ops/p4-stable-instance-host.md) | en | `docs/ops/p4-stable-instance-host.md` |
| [PMF Golden Scenario — verified from a browser](ops/pmf-golden-scenario-acceptance.md) | en | `docs/ops/pmf-golden-scenario-acceptance.md` |
| [PocketLLM GPU demo — turnkey host setup (Store → Run → chat)](ops/pocketllm-demo-host.md) | en | `docs/ops/pocketllm-demo-host.md` |
| [Datasette portable Python/OCI staging acceptance — 2026-09-16](ops/portable-datasette-derivations-staging-2026-09-16.md) | en | `docs/ops/portable-datasette-derivations-staging-2026-09-16.md` |
| [Datasette dependency export progress — 2026-09-16](ops/portable-dependency-export-progress-2026-09-16.md) | en | `docs/ops/portable-dependency-export-progress-2026-09-16.md` |
| [Portable durable local Instance progress — 2026-09-17](ops/portable-durable-local-instance-progress-2026-09-17.md) | en | `docs/ops/portable-durable-local-instance-progress-2026-09-17.md` |
| [Portable Instance snapshot progress — 2026-09-17](ops/portable-instance-snapshot-progress-2026-09-17.md) | en | `docs/ops/portable-instance-snapshot-progress-2026-09-17.md` |
| [Public SMTP staging acceptance — prerequisites and blockers, 2026-09-20](ops/public-smtp-staging-prerequisites-2026-09-20.md) | en | `docs/ops/public-smtp-staging-prerequisites-2026-09-20.md` |
| [b11429 offload証拠の調査・修正（追加有料Podなし）](ops/runpod-offload-evidence-b11429-2026-10-09.md) | ja+en | `docs/ops/runpod-offload-evidence-b11429-2026-10-09.md` |
| [RunPod初回503の調査と起動準備・診断の修正](ops/runpod-startup-diagnostics-2026-10-09.md) | ja+en | `docs/ops/runpod-startup-diagnostics-2026-10-09.md` |
| [Runtime Network 3c-a search budget — local acceptance, 2026-09-25](ops/runtime-network-3c-a-acceptance-2026-09-25.md) | en | `docs/ops/runtime-network-3c-a-acceptance-2026-09-25.md` |
| [Runtime Network 3c-b acceptance — 2026-09-25](ops/runtime-network-3c-b-2026-09-25.md) | en | `docs/ops/runtime-network-3c-b-2026-09-25.md` |
| [3c-b final archive hardening — 2026-09-25](ops/runtime-network-3c-b-hardening-2026-09-25.md) | en | `docs/ops/runtime-network-3c-b-hardening-2026-09-25.md` |
| [Runtime Network Phase 1 — acceptance (2026-09-23)](ops/runtime-network-phase1-acceptance-2026-09-23.md) | en | `docs/ops/runtime-network-phase1-acceptance-2026-09-23.md` |
| [Native-inference GPU (SGLang / CUDA) — Ubuntu NVIDIA runbook](ops/sglang-cuda-host.md) | en | `docs/ops/sglang-cuda-host.md` |
| [Single OCI Model Set profile acceptance — 2026-10-24](ops/single-oci-model-sets-20261024.md) | en | `docs/ops/single-oci-model-sets-20261024.md` |
| [Explicit source provenance in the existing Requester client](ops/source-runtime-provenance-client-20261024.md) | en | `docs/ops/source-runtime-provenance-client-20261024.md` |
| [Staging orphaned lease recovery — 2026-09-27](ops/staging-orphaned-lease-recovery-2026-09-27.md) | en | `docs/ops/staging-orphaned-lease-recovery-2026-09-27.md` |
| [Staging snapshot builder — ubuntu-sugamo runbook](ops/staging-snapshot-builder-sugamo.md) | en | `docs/ops/staging-snapshot-builder-sugamo.md` |
| [Stalwart mail connectivity decision — 2026-09-20](ops/stalwart-mail-connectivity-decision-2026-09-20.md) | en | `docs/ops/stalwart-mail-connectivity-decision-2026-09-20.md` |
| [Stalwart + Missive staging acceptance — 2026-09-20](ops/stalwart-missive-mail-staging-2026-09-20.md) | en | `docs/ops/stalwart-missive-mail-staging-2026-09-20.md` |
| [Stateful transformation v1 — E2E evidence](ops/stateful-transformation-v1.md) | ja+en | `docs/ops/stateful-transformation-v1.md` |
| [Static Web instance state lane — staging builder record](ops/static-web-instance-state-lane.md) | en | `docs/ops/static-web-instance-state-lane.md` |
| [Step 10 — Process Personal App, staging acceptance](ops/step10-process-personal-app-acceptance.md) | en | `docs/ops/step10-process-personal-app-acceptance.md` |
| [Trilium Notes staging acceptance — 2026-09-20](ops/trilium-notes-staging-2026-09-20.md) | ja+en | `docs/ops/trilium-notes-staging-2026-09-20.md` |
| [Publishing your app to the Ato Store](legacy/publishing-to-ato-store.md) | en | `docs/publishing-to-ato-store.md` |
| [Connected Runner](legacy/runner.md) | en | `docs/runner.md` |
| [Sandbox](legacy/sandbox.md) | en | `docs/sandbox.md` |
| [Snapshot v1 Compatibility Contract](legacy/snapshot-v1-compatibility.md) | en | `docs/snapshot-v1-compatibility.md` |
| [Launch Preparation Bridge Contract (#581 ↔ #593)](legacy/launch-preparation-plan.md) | en | `docs/specs/launch-preparation-plan.md` |
