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
- 次の推奨着手: 05: run の lock-first 化
- その次: 06: init の durable workspace materialization 化

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
