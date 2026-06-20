# Ticket 03: Manifest And Legacy Lock Compiler

- Status: Proposed
- Priority: P0
- Depends on: 01, 02
- Blocks: 04, 05, 06, 07, 08, 09

## Goal

`capsule.toml` と既存 `capsule.lock.json` を import source として読み、canonical な lock-shaped IR へ変換する compiler を実装する。

## Scope

- manifest -> lock-shaped IR compiler
- existing `capsule.lock.json` importer
- partially resolved marker の first-class 化
- source mapping / provenance の保持

## Out Of Scope

- source-only inference
- final execution plan 生成

## Required Outcomes

- compatibility input から downstream が lock-shaped model を読むようにできる
- unresolved state を silent omission ではなく marker と reason class で表現できる
- v0.2 / v0.3 / CHML-like path を import source として吸収できる

## Implementation Slices

1. manifest importer
2. legacy lock importer
3. compatibility compiler
4. unresolved marker 共通型
5. provenance model
6. compiler diagnostics

## Acceptance Criteria

- `capsule.toml` から最低限 `contract` と `resolution` skeleton を生成できる
- 既存 `capsule.lock.json` から runtime / dependency / injected data の補助情報を取り込める
- ambiguity や不足情報を unresolved marker で保持できる
- compiler 出力を 04 と 05 が入力として再利用できる

## Primary Touchpoints

- 新規 `apps/ato-cli/src/application/compat_import/*`
- `apps/ato-cli/core/src/types/manifest.rs`
- `apps/ato-cli/core/src/types/manifest_v03.rs`
- `apps/ato-cli/core/src/lockfile.rs`

## Open Questions

- manifest 由来の `services` / `targets` をどのレベルで `contract.workloads` へ寄せるか
- 既存 `capsule.lock.json` のどこまでを `resolution` に昇格させるか
