# ato.lock / Source Inference Implementation Tickets

このディレクトリは、次の 2 件の ADR を実装へ落とすためのチケット群を管理する。

- ADR: ato.lock.json As Canonical Input
- ADR: Source Inference Model For ato run / ato init

## 目的

- manifest-first から lock-first への段階移行を、既存の hourglass pipeline を壊さずに進める
- `ato run` と `ato init` の source-started path を共通の source inference pipeline に統一する
- `ato.lock.json` を canonical input とし、execution plan / `config.json` を派生物へ整理する

## チケット一覧

1. [01-ato-lock-model-and-canonicalization.md](./01-ato-lock-model-and-canonicalization.md)
2. [02-input-resolver-and-dual-path.md](./02-input-resolver-and-dual-path.md)
3. [03-manifest-and-legacy-lock-compiler.md](./03-manifest-and-legacy-lock-compiler.md)
4. [04-shared-source-inference-engine.md](./04-shared-source-inference-engine.md)
5. [05-run-lock-first-entry.md](./05-run-lock-first-entry.md)
6. [06-init-durable-workspace-materialization.md](./06-init-durable-workspace-materialization.md)
7. [07-execution-plan-and-config-from-lock.md](./07-execution-plan-and-config-from-lock.md)
8. [08-validate-install-integration.md](./08-validate-install-integration.md)
9. [09-build-publish-registry-migration.md](./09-build-publish-registry-migration.md)
10. [10-inspect-preview-remediation-surface.md](./10-inspect-preview-remediation-surface.md)
11. [11-binding-policy-attestations-state-management.md](./11-binding-policy-attestations-state-management.md)

## 現在の実装状況

- 完了: 01: ato.lock モデルと canonicalization
- 完了: 02: input resolver と dual-path 境界
- 完了: 03: manifest / legacy lock から lock-shaped IR への compiler
- 完了: 04: shared source inference engine
- 完了: 05: run の lock-first 化
- 完了: 06: init の durable workspace materialization 化
- 完了: 07: execution plan / config.json の lock-derived 化
- 完了: 09: build / publish / registry key の移行
- 完了: 11: binding / policy / attestations state management の明確化
- 次の推奨着手: 08: validate / install の lock-first 化
- その次: 10: inspect / preview / remediation surface の lock-path 化

Ticket 01 では `core/src/ato_lock/` を追加し、少なくとも次を実装済み。

- `ato.lock.json` v1 の基礎スキーマ
- canonical projection と `lock_id` の deterministic hash
- draft 向け structural validation と persisted artifact 向け validation の分離
- unresolved marker / feature / signature placeholder の基礎モデル
- focused unit tests による canonicalization / validation / draft-to-persisted path の検証

Ticket 02 では authoritative input の境界を `core/src/input_resolver.rs` に集約し、少なくとも次を実装済み。

- `ResolvedInput` を file kind ではなく project state として定義
- discovery / classification / materialization / advisories の責務分離
- `ato.lock.json` 優先、invalid canonical lock は fail-closed、`capsule.lock.json` 単独も fail-closed
- `validate` / `run` / `build` / `publish` / `init` 入口で resolver を呼ぶ共通境界の導入
- provenance と advisory の返却、および focused unit test / CLI test による precedence 検証

### Ticket 02 実装報告

- 完了日: 2026-03-25
- 追加した境界: `ato.lock.json` / compatibility project / source-only を共通解決する resolver
- 固定したポリシー: invalid `ato.lock.json` が存在する場合は compatibility input へ暗黙 fallback しない
- 入口反映: `validate` は authoritative input を直接解決し、`run` / `build` / `publish` / `init` は canonical input 検出時に fail-closed で止まる入口へ変更
- 既知の残作業: canonical lock を実際の実行・build・publish 入力として消費する downstream 実装は Ticket 03 / 05 / 06 / 08 / 09 で継続

### Ticket 03 実装報告

- 完了日: 2026-03-25
- 追加した境界: `src/application/compat_import/*` に compatibility compiler を新設し、resolver の authoritative input classification と分離
- manifest import: `service -> contract.workloads`、`target/runtime/runtime_version -> resolution` hint、single-process のみ `contract.process` を deterministic に設定
- legacy lock import: runtime / tools / dependency / injected data / target artifact を `resolution` enrich only として取り込み、manifest-derived `contract` は上書きしない
- draft guarantee: compiler 出力は execution-usable canonical lock ではなく、downstream resolution / diagnostics 用の lock-shaped draft として明示
- provenance/diagnostics: semantic unresolved は draft lock、path-aware explanation は provenance / diagnostics sidecar に分離
- focused tests: single-service / multi-service / CHML-like / legacy runtime conflict / deterministic ordering を検証済み

### Ticket 03 Follow-up Checks

- `resolution` hint shape は Ticket 04/05 の受け口に概ね整合: `resolved_targets`、`runtime_hints`、`target_selection` が source inference / run handoff の最小単位として使える形を維持
- `contract.process` provenance を確認: single-service では選ばれた process の由来を provenance で追跡でき、multi-service では ambiguity が diagnostics / unresolved に残る
- `injected_data` は `resolution.locked_injected_data` に限定: durable contract semantics へ昇格せず、legacy lock supplemental data であることを provenance note でも区別
- manifest only の draft は deterministic: legacy lock なしでも同一入力で同一 draft / diagnostics / provenance を再現
- service の source order は workload ordering に影響しない: imported workload は service 名で安定化
- legacy conflict は `contract` を汚染しない: conflict は `resolution` / unresolved / diagnostics 側だけに現れ、manifest-derived contract は不変

### Ticket 04 実装報告

- 完了日: 2026-03-25
- 追加した境界: `src/application/source_inference/mod.rs` に shared source inference engine を追加し、`SourceEvidence` / `DraftLock` / `CanonicalLock` を infer -> resolve -> materialize の共通入口で扱うようにした
- canonical lock handoff: canonical input は materialize 起点としてのみ扱い、`run` / `init` ともに semantic re-inference を行わない
- compatibility draft handoff: Ticket 03 compiler が作った draft lock の `contract.process` は shared engine で再推論せず、そのまま durable materialization に引き継ぐ
- `run` 入口反映: source-only と canonical lock は shared inference で attempt-local の `ato.lock.json` / provenance sidecar / generated manifest bridge を生成して既存 hourglass pipeline へ接続する
- `init` 入口反映: source-only と compatibility project は shared inference で workspace-local の durable `ato.lock.json` と provenance sidecar を生成する
- ambiguity policy: equal-rank process candidate は `run` では fail-closed、`init` では unresolved marker として durable lock に保持する
- sidecar policy: `run` は `.tmp/source-inference/<attempt>/`、`init` は `.ato/source-inference/` に provenance sidecar を書き分ける
- focused tests: `compatibility_draft_handoff_does_not_reinfer_process` と `source_inference::tests` を通し、source-only inference、draft handoff、generated manifest materialization、unresolved durability、equal-rank fail-closed を検証済み

### Ticket 04 Follow-up Checks

- `run` compatibility path は未統一: `src/cli/commands/run.rs` では compatibility project のみ既存 `capsule.toml` をそのまま prepare phase へ渡しており、shared inference 完全統一は Ticket 05 の明示的 technical debt とする
- generated manifest bridge の semantic safety を確認: shared engine が生成する manifest は `contract.process.entrypoint` と `cmd` から派生した bridge artifact に限定され、下流の `run_prepare_phase` はその manifest を直接 load するだけで authoritative input resolver や source inference を再実行しない
- `run` execute precondition を engine 側で確認: `process`、`resolution.runtime`、`resolution.resolved_targets`、`resolution.closure` が欠ける場合は `execute_shared_engine(..., RunAttempt, ...)` が `AtoExecutionError` で停止し、execute phase へ進まない
- durable safety boundary を確認: current source-only durable output は `contract.metadata`、`network`、`env_contract`、`filesystem`、および `resolution.runtime` / `resolved_targets` / `closure` に限定され、secret value、identity provider、privileged write、approval 前提の externally exposed network semantics は shared inference で昇格させていない
- approval gate は placeholder のまま: approval-required path を表す型はあるが、現時点では未実装であり、script-capable resolution は fail-closed のまま残す
- command integration はまだ部分的: existing execution path は generated manifest bridge を介した manifest-based routing のままで、true lock-first downstream consumption は Ticket 05/07 で継続する
- 残る warnings は将来フックと旧 helper の混在によるもの: unused field / enum variant / old init helper が残っており、次チケット前に intentional future hook と dead code を整理する余地がある

### Ticket 05 実装報告

- 完了日: 2026-03-25
- 追加した境界: `src/cli/commands/run.rs` と `src/application/pipeline/phases/run.rs` で `RunAuthoritativeInput` / `PreparedRunContext` を導入し、`ato run` の authoritative input を install -> prepare -> build -> verify -> dry_run -> execute まで明示的に伝搬するようにした
- compatibility path 統一: compatibility project も shared source inference を通して attempt-local の `ato.lock.json` / provenance sidecar / generated manifest bridge を materialize し、canonical lock / source-only と同じ run handoff に統一した
- prepared-context ownership: downstream executor / preflight / orchestrator は `PreparedRunContext` の `raw_manifest` / `validation_mode` / `engine_override_declared` / compatibility-scoped legacy lock context を使い、manifest を直接 reread して authority を再解釈しないようにした
- legacy lock boundary: `capsule.lock.json` の整合性確認と external dependency 検証は explicit compatibility context でのみ行い、generated bridge manifest ではなく original compatibility manifest hash に対して fail-closed で照合するようにした
- bridge safety: generated manifest bridge は selected target の `runtime` / `driver` / `runtime_version` / `cmd` / `required_env` と manifest-compatible `network` policy を保持し、web static path validation や execution consent を落とさないようにした
- compatibility runtime promotion: draft lock の `resolution.target_selection` / `runtime_hints` / `locked_runtimes` から execution-ready `resolution.runtime` を昇格させる bounded bridge rule を shared source inference に追加した
- closure policy: compatibility draft に dependency closure が欠ける場合は metadata-only observed lockfile state を合成し、execute precondition を満たさない canonical lock は fail-closed のまま維持した
- fail-closed coverage: runtime unresolved / resolved targets missing / closure missing / bridge hash mismatch / prepared-context no-reread / compatibility lock hash mismatch / missing consent / web path traversal の focused test を追加または更新した

### Ticket 05 Follow-up Checks

- compatibility runtime promotion は canonical semantic override ではない: runtime 決定は compatibility draft の target selection と locked runtime hint からのみ補完し、canonical lock の resolved runtime は再推論しない
- metadata-only closure は execution-ready dependency graph を主張しない: lockfile presence を観測した minimal marker に限定し、closure 自体が欠ける canonical lock は `execute_shared_engine(..., RunAttempt, ...)` で fail-closed のまま止まる
- prepared manifest ownership を確認: `target_runner::resolve_launch_context`, `prepare_target_execution`, `preflight_native_sandbox`, `orchestrator::launch_service` は `PreparedRunContext` を受け取り、run downstream で manifest reread が不要になった
- policy semantics が bridge で失われないことを確認: compatibility-generated bridge manifest は selected target runtime semantics と top-level network policy を保持し、consent gate と web static path canonicalization が引き続き発火する
- fail-closed manifest policy suite を確認: `tests/fail_closed_manifest_policy.rs` は unix 上で mock nacelle を使って E205 を回避しつつ、missing consent / `--yes` bypass denial / web entrypoint traversal rejection を再び検証できる状態に戻した
- full-suite 確認済み: `cargo test -p ato-cli --test fail_closed_manifest_policy -- --nocapture` と `cargo test -p ato-cli` が通過した

### Ticket 06 実装報告

- 完了日: 2026-03-25
- 追加した境界: `src/cli/root.rs` / `src/cli/dispatch/mod.rs` で top-level `ato init` を durable-first に切り替え、通常系は workspace-local durable materialization、legacy manifest scaffold は `--legacy prompt` / `--legacy manual` に明示分離した
- transitional caller policy: `build --init` と local run manifest recovery は `write_legacy_detected_manifest(...)` に退避し、移行期間中も `capsule.toml` 前提の既存 caller を壊さないようにした
- workspace materializer: `src/application/workspace/init/materialize.rs` を新設し、repo-tracked durable output を `ato.lock.json` に固定したうえで、workspace-local state として `.ato/source-inference/provenance.json`、`.ato/source-inference/provenance-cache.json`、`.ato/binding/seed.json` を書き出す責務を集約した
- shared source inference handoff: `src/application/source_inference/mod.rs` の workspace write は materializer に委譲し、inference は semantic state の生成、materializer は durable/workspace state の永続化に責務を分離した
- durable partial validation: persisted 前に `contract.process`、`resolution.runtime`、`resolution.resolved_targets`、`resolution.closure` の各 execution-critical field について、resolved value か inspectable unresolved marker のどちらかを必須にし、`binding.entries` / `attestations.entries` の canonical lock への埋め込みを禁止した
- workspace-local side state policy: `provenance-cache.json` と `binding/seed.json` は inspect / preview / remediation 向け handoff 面として置く internal workspace state であり、現時点では repo-tracked public contract や backward-compatible external schema としては扱わない
- CLI/doc 反映: README / README_JA / `docs/current-spec.md` を durable `ato init` 前提へ更新し、`ato init` と `build --init` の意味差を warning text と説明文で明示した
- focused tests: `init_command_defaults_to_durable_workspace_materialization`、`init_command_parses_legacy_modes`、`materialize_workspace_writes_cache_and_binding_seed`、`durable_workspace_lock_*` を追加または更新し、CLI 契約と durable workspace materialization の最小保証を確認した

### Ticket 06 Follow-up Checks

- transitional UX を確認: top-level `ato init` は `ato.lock.json` を materialize する durable path、`build --init` は inferred `capsule.toml` を作る legacy compatibility path のままであり、`src/cli/commands/build.rs` の warning text でもこの差を明示している
- legacy escape hatch を確認: `InitLegacyMode` により prompt/manual scaffold は explicit opt-in になっており、durable path に戻したい caller と legacy manifest path を保ちたい caller が CLI 契約で区別できる
- durable partial boundary を確認: validator は `process` だけでなく `runtime` / `resolved_targets` / `closure` も同じく checked しており、shared inference 側の既存 safety boundaryにより secret value、identity provider、privileged write、approval 前提 network semantics は引き続き durable contract へ昇格していない
- provenance handoff shape を確認: `provenance-cache.json` には `input_kind`、lock / provenance / binding seed の path、`lock_id`、`generated_at`、unresolved summary、field provenance index、diagnostics count を保持しており、Ticket 10 の inspect/preview consume 側が最低限必要とする索引面を先に確保している
- workspace-local schema posture を確認: `binding/seed.json` と `provenance-cache.json` はどちらも `schema_version` を持つが、現時点では `.ato/` 配下の internal state としてのみ扱い、public API 的な互換保証はまだ置かない
- 現時点の confidence は focused tests ベース: `cargo test -p ato-cli init_command -- --nocapture`、`cargo test -p ato-cli materialize_workspace -- --nocapture`、`cargo test -p ato-cli durable_workspace_lock -- --nocapture` は通過したが、command-level の `ato init` E2E はまだ未追加であり、full end-to-end confidence は Ticket 06 follow-up または Ticket 10 着手前に補強余地がある

### Ticket 07 実装報告

- 完了日: 2026-03-25
- 追加した境界: `core/src/lock_runtime.rs` に shared selection / runtime model を追加し、`core/src/execution_plan/derive.rs` と `core/src/r3_config.rs` から同じ lock-derived selection 結果を共有して plan / `config.json` を導出するようにした
- execution plan lock-first 化: `compile_execution_plan_from_lock(...)` を追加し、authoritative lock がある `run` 経路では manifest semantic parse ではなく canonical lock の `contract` / `resolution` から execution plan を再生成するようにした
- config lock-first 化: `generate_config_from_lock(...)` を追加し、authoritative lock がある source 実行経路では `config.json` も lock-derived service/runtime/network 情報から再生成するようにした
- run path integration: `PreparedRunContext` に `authoritative_lock` を保持し、`target_runner` / source executor / orchestrator まで明示伝搬して downstream で authority を再解釈しない境界を維持した
- launch-time compatibility boundary: generated bridge manifest や prepared raw manifest は semantic source ではなく launch-time compatibility data としてのみ使い、`[ipc]` のような non-schema section は original raw TOML から保持するようにした
- consent hash posture: consent key shape (`scoped_id` / `version` / `target_label`) は維持しつつ、policy / provisioning hash は lock-derived plan から計算する経路へ寄せた
- broad regression fixes: lock-first 化で顕在化した IPC fail-closed 回帰、web/static path traversal の fail-closed 順序不整合、local registry publish path の nested Tokio runtime panic、local registry consent test の不安定性を修正した
- full-suite 確認済み: `cargo test -p ato-cli` が通過し、IPC / local registry / publish / policy / run path を含む broad regression が green に戻っていることを確認した

### Ticket 07 Follow-up Checks

- semantic source boundary を確認: authoritative input が canonical lock の場合、execution plan / `config.json` の semantic derive は lock-first compiler が担い、generated/raw manifest は launch-time compatibility data に限定される
- shared selection result を確認: plan compiler と config compiler はどちらも `ResolvedLockRuntimeModel` を受け取り、selected target / services / network の concrete selection を別々に再選択しない
- incomplete draft fail-closed を確認: `contract.process`、`resolution.runtime`、`resolution.resolved_targets`、`resolution.closure` のいずれかが不足する lock は execution-ready とみなさず、lock-derived 実行経路で fail-closed になる
- overlay / ambient boundary を確認: compiler 側は lock-derived selection と explicit overlay だけを入力にし、ambient host discovery や launch-time file existence を semantic fallback として使わない
- bridge safety を確認: compatibility/source inference bridge は selected target runtime semantics と top-level network policy に加えて non-schema launch-time section (`[ipc]`) を保持するが、semantic authority 自体は lock-first compiler 側に残す
- residual follow-up: consent hash invalidation の専用回帰テスト、guard / ambient discovery 境界の明示テスト、web/static outside-manifest の error code 整理は Ticket 07 完了後の品質向上項目として残る

### Ticket 09 実装報告

- 完了日: 2026-03-25
- 追加した境界: `src/application/producer_input.rs` に producer 用 authoritative input resolver を導入し、`build` / private publish / official publish / CI publish が canonical lock / compatibility project / source-only を同じ入口で解決するようにした
- build / publish integration: `src/cli/commands/build.rs` と `src/application/pipeline/phases/publish.rs` / `src/cli/dispatch/publish.rs` が producer authoritative input を経由するようになり、canonical `ato.lock.json` を build/publish の実入力として扱えるようにした
- bridge safety: shared source inference が生成する manifest bridge は既存 pack/build/publish 実装へ接続する compatibility artifact のまま維持し、producer path の semantic authority は `ResolvedInput` とそこから materialize された lock に残した。canonical lock path では original manifest を bridge へ注入せず、compatibility path でも repository / `[build]` / `[store]` / `[ipc]` の launch-time compatibility 情報だけを保持する
- registry metadata bridge: `PublishableArtifact` と local registry upload/store 経路に optional `lock_id` / `closure_digest` を追加し、`registry_releases` へ additive nullable column として保存するようにした。一方で release 解決と manifest 検証の主キーは引き続き `manifest_hash` を使い、rekey は将来移行のための補助 metadata として段階導入した
- optional semantics: source/build 起点 publish では materialized lock から `lock_id` と `resolution.closure` digest を計算して forward し、既存 artifact 再公開のように lock provenance を復元できない経路では `None` のまま通す。欠落は schema drift ではなく「未供給/未導出」を表す
- regression coverage: `tests/cli_tests.rs` の canonical build success regression、`src/adapters/registry/store/tests.rs` の lock metadata persistence regression、`tests/local_registry_e2e.rs` の canonical private publish E2E を通して、producer authoritative input と registry metadata bridge の最小保証を固定した

### Ticket 09 Follow-up Checks

- generated manifest bridge の semantic boundary を確認: `resolve_producer_authoritative_input(...)` は常に authoritative input を先に解決し、その結果から一時 manifest を作る。private publish 実行時も `src/cli/dispatch/publish.rs` 側で再度 producer authoritative input を解決して lock metadata を付与しており、bridge manifest 自体を semantic source へ戻していない
- `lock_id` / `closure_digest` の optional semantics を確認: `src/application/producer_input.rs` では `compute_lock_id(...)` と `resolution.entries["closure"]` の digest 化に成功した場合だけ値を埋め、そうでない入力は `None` のまま downstream へ流す。registry serve/store 側も nullable のまま保持し、未設定をエラー扱いしない
- artifact identity との共存を確認: `src/adapters/registry/store/mod.rs` は publish 時に capsule から検証した `manifest_hash` を引き続き release row と resolve join に使っており、`lock_id` / `closure_digest` は conflict 判定や current release 解決を置き換えていない
- private publish E2E を確認: `e2e_local_registry_private_publish_prefers_canonical_lock_metadata` では manifest と canonical lock の name/version を意図的に食い違わせ、private publish 後の registry detail が lock 由来の `canonical-publish@0.4.2` と `lock_id` / `closure_digest` を返し、manifest 側の `ignored-manifest` は公開されないことを検証した

### Ticket 11 実装報告

- 完了日: 2026-03-26
- 追加した境界: `src/application/workspace/state.rs` を新設し、workspace-local mutable state (`binding seed` / `policy bundle` / `attestation store`) の path/schema/read boundary を 1 箇所へ集約した
- binding precedence: effective binding は `CLI > workspace-local > embedded` として `resolve_effective_lock_state(...)` で一度だけ解決し、`RunAuthoritativeInput` 経由で run pipeline に伝搬するようにした
- policy precedence: effective policy は `workspace-local > embedded > default local allow` とし、default allow は「追加の local restriction が無い」ことだけを意味し、lock-derived contract / execution plan / `config.json` が持たない capability を付与しないようにした
- deny-wins enforcement: workspace policy は `validate_execution_plan_against_policy(...)` と `validate_config_against_policy(...)` で plan / generated config の両方に fail-closed で適用し、deny が allow より常に優先されるようにした
- init integration: durable `ato init` は `.ato/policy/bundle.json` と `.ato/attestations/store.json` を `.ato/source-inference/*` / `.ato/binding/seed.json` と合わせて初期生成するようにした
- attestation semantics: empty attestation store は「approval / observation がまだ 1 件も記録されていない」ことを表し、それ自体は grant を意味しない
- distribution boundary: producer/publish 側では generated `ato.lock.json` を `sanitize_lock_for_distribution(...)` で再書き込みし、`binding` / `attestations` だけを落として distribution へ流すようにした。embedded `policy` は distributable lock の execution allowability を表すため保持し、workspace-local policy bundle 自体は distribution に載せない
- reusable posture: effective state の解決と sanitize は run 専用 helper に埋め込まず `state.rs` に閉じ込め、将来の build / publish / inspect handoff でも再利用できる形にした
- focused verification: `init_command_defaults_to_durable_workspace_materialization`、`materialize_workspace_writes_cache_and_binding_seed`、`binding_precedence_prefers_cli_then_workspace_then_embedded`、`workspace_policy_bundle_overrides_embedded_policy_source`、`policy_deny_wins_over_allow`、`sanitize_lock_for_distribution_strips_binding_and_attestations`、`e2e_local_registry_private_publish_prefers_canonical_lock_metadata` を通して、init / state precedence / sanitize / private publish 境界の最小保証を確認した

### Ticket 11 Follow-up Checks

- `default local allow` の意味を確認: local policy が存在しない場合でも、lock-derived plan / config に含まれない capability を許可するわけではなく、あくまで追加の workspace-local deny/allow 制約が無いことだけを表す
- empty attestation store の意味を確認: `.ato/attestations/store.json` の初期空状態は未記録状態であり、approval の implicit grant ではない
- sanitize 後に policy を残す意図を確認: distribution では mutable local section のみを落とし、embedded policy は execution allowability の一部として残す。一方で workspace-local policy bundle は `.ato/` 配下の internal state であり distribution artifact に同梱しない
- reusable boundary を確認: effective binding/policy 解決は `state.rs` 側に閉じており、run pipeline はその結果を受け取るだけなので、将来の build / publish / inspect 側にも持ち上げやすい
- confidence の現状を確認: focused unit / integration / local-registry E2E までは確認済みだが、専用の `ato init` command-level E2E と distribution sanitize 専用 E2E はまだ未追加であり、運用境界の confidence 補強余地は残る

## 推奨実装順

### Wave 1

- 01: ato.lock モデルと canonicalization
- 02: input resolver と dual-path 境界
- 03: manifest / legacy lock から lock-shaped IR への compiler

### Wave 2

- 04: shared source inference engine
- 05: run の lock-first 化
- 06: init の durable workspace materialization 化

### Wave 3

- 07: execution plan / config.json を lock-derived に移行
- 08: validate / install の lock-first 化
- 09: build / publish / registry key の移行
- 10: inspect / preview / remediation surface の lock-path 化
- 11: binding / policy / attestations state management の明確化

## 実装ポリシー

- 既存の hourglass pipeline は維持する
- `ato.lock.json` が存在する場合は canonical input として最優先する
- compatibility input は import source として扱い、暗黙マージしない
- partially resolved durable lock を許容するが、unresolved state は first-class marker で表現する
- `binding` / `policy` / `attestations` は canonical reproducibility projection から分離する

## 完了の定義

最低限、次が成立した時点を lock-first migration の中間完了とする。

- `ato run` が source input からでも canonical lock-shaped input を合成して実行できる
- `ato init` が durable な `ato.lock.json` を生成できる
- execution plan と `config.json` が lock-derived input から再生成できる
- validate / install が canonical input resolver を経由する
- inspect / preview / remediation が lock path と provenance を前提に動作する
- binding / policy / attestations の precedence と既定保存戦略がコード境界として固定される
