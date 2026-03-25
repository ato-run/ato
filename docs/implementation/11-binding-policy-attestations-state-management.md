# Ticket 11: Binding / Policy / Attestations State Management

- Status: Proposed
- Priority: P2
- Depends on: 01, 05, 06, 07
- Blocks: 08, 09

## Goal

`binding`, `policy`, `attestations` の precedence, persistence, read boundary を固定し、repo-tracked reproducibility core と host-local mutable state の混線を防ぐ。

## Scope

- binding source precedence の実装
- workspace-local state layout の設計
- policy input source と deny/allow semantics の固定
- attestations の既定保存戦略と read/write boundary の整理

## Out Of Scope

- organization policy distribution の最終プロトコル
- approval record の最終署名フォーマット
- registry wire format

## Required Outcomes

- execution plan / config / run / install / publish が同じ precedence で binding / policy を読む
- workspace-local binding seed と cache の役割が分離される
- embedded `binding` と `attestations` は opt-in かつ既定除外の境界が固定される
- policy の差異は execution 可否に影響しても `lock_id` に影響しないことがコード境界として維持される

## Implementation Slices

1. binding precedence model 実装
2. workspace-local state directory / schema 設計
3. policy bundle read boundary 実装
4. attestations read/write policy 実装
5. install / publish / export での embedded state exclusion ルール適用

## Acceptance Criteria

- binding source precedence が `CLI > workspace-local state > embedded binding > unresolved/runtime default` で一貫する
- workspace-local binding seed は canonical execution state として扱われない
- policy evaluation が deny 優先で動き、contract が policy を超えた場合 fail-closed する
- attestations は既定で repo-tracked canonical lock content の外側に保持される
- publish / export path で embedded `binding` と `attestations` が既定除外され、明示 opt-in でのみ含まれる

## Primary Touchpoints

- binding loader / state store
- policy evaluation path
- attestation store
- install / publish / export pipeline
- execution plan / config materialization boundary

## Open Questions

- workspace-local state を単一ディレクトリに集約するか機能ごとに分割するか
- approval-derived attestations と observational attestations を同一ストアで持つか分けるか
