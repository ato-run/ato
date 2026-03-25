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
