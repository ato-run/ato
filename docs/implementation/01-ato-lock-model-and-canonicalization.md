# Ticket 01: ato.lock Model And Canonicalization

- Status: Proposed
- Priority: P0
- Depends on: none
- Blocks: 02, 03, 04, 05, 06, 07, 08, 09

## Goal

`ato.lock.json` の on-disk schema と in-memory model を定義し、canonical projection / `lock_id` / strict validation の基盤を実装する。

## Scope

- `ato.lock.json` 用 serde 型の追加
- canonical projection の定義
- `lock_id` 計算実装
- strict validation と feature validation
- parser / serializer / canonical serializer の導入

## Out Of Scope

- manifest からの import
- source inference
- execution plan 生成
- registry wire format

## Required Outcomes

- `schema_version = 1` の `ato.lock.json` を parse / validate できる
- `lock_id` が `schema_version + resolution + contract` の projection から決定的に計算される
- `binding`, `policy`, `attestations`, `signatures`, `generated_at` が canonical hash scope から除外される
- unsupported feature を strict mode で fail-closed にできる

## Implementation Slices

1. 新規モジュール追加
   - 例: `core/src/ato_lock/mod.rs`
   - `schema.rs`, `validate.rs`, `canonicalize.rs`, `hash.rs` に分離
2. Top-level schema 実装
   - `schema_version`
   - `lock_id`
   - `generated_at`
   - `features`
   - `resolution`
   - `contract`
   - `binding`
   - `policy`
   - `attestations`
   - `signatures`
3. Canonical projection 実装
4. `lock_id` 再計算検証実装
5. strict / non-strict validator 実装
6. 初期ユニットテスト追加

## Acceptance Criteria

- `ato.lock.json` 単体の load / serialize / canonical hash のユニットテストが揃う
- `lock_id` 再計算失敗時に fail-closed する
- `binding` の変更だけでは `lock_id` が変わらない
- `policy` の変更だけでは `lock_id` が変わらない
- `contract` または `resolution` の変更では `lock_id` が変わる

## Primary Touchpoints

- 新規 `apps/ato-cli/core/src/ato_lock/*`
- 既存 JCS 資産の再利用箇所

## Open Questions

- `features.required_for_execution` の初期 enum をどこまで固定するか
- `unresolved marker` の共通表現を top-level schema に寄せるか section ごとに持つか
