# Ticket 09: Build / Publish / Registry Migration

- Status: Proposed
- Priority: P3
- Depends on: 01, 03, 07, 08, 11
- Blocks: none

## Goal

producer flow と registry metadata を lock-first へ移し、将来的な `lock_id` / `closure_digest` ベースの配布設計へつなぐ。

## Scope

- build の lock-first 入力対応
- publish の lock-first 入力対応
- registry key 再編の設計と段階実装
- manifest_hash 依存の縮退
- producer flow での embedded mutable state exclusion ルール適用

## Out Of Scope

- registry API の最終 wire format 完了
- signature block の最終仕様確定

## Required Outcomes

- build / publish が canonical input resolver を経由する
- producer flow の Prepare / Build / Verify / Install / Dry-run / Publish が lock-first world で動く
- registry key を `manifest_hash` 中心から `lock_id` / `closure_digest` 中心へ移すための移行土台を作る
- publish / export artifact が embedded `binding` / `attestations` を既定除外できる

## Implementation Slices

1. build command lock-first integration
2. publish command lock-first integration
3. artifact identity / provenance rekey design
4. signing / hashing boundary adaptation
5. registry compatibility bridge
6. publish / export mutable state exclusion rule 適用

## Acceptance Criteria

- build / publish が `ato.lock.json` を authoritative input として扱える
- compatibility input は import source 扱いに留まる
- artifact identity の移行計画がコード上の boundary と一致する
- 既存 registry / publish path を段階移行できる構造になる
- publish artifact の既定出力に embedded `binding` / `attestations` が混入しない

## Primary Touchpoints

- build / publish pipeline
- registry metadata
- signing / packers
- store / distribution adapters

## Open Questions

- `lock_id` と `closure_digest` をどの順で registry key に導入するか
- publish artifact に embedded `binding` を既定除外する境界をどこへ置くか
