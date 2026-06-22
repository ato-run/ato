# Ticket 07: Execution Plan And config.json From Lock

- Status: Proposed
- Priority: P1
- Depends on: 01, 03, 05, 06
- Blocks: 08, 09, 10, 11

## Goal

execution plan と `config.json` の導出元を manifest-first から lock-first へ切り替える。

## Scope

- execution plan from lock
- config from lock
- runtime materialization detail の分離
- ambient host state 依存の削減
- binding / policy input boundary の固定

## Out Of Scope

- registry wire format
- final publish key redesign

## Required Outcomes

- execution plan が `ato.lock.json` と明示的 binding / policy input から決定的に再生成できる
- `config.json` が派生 IR として残る
- manifest 直読みが plan 生成の必須前提でなくなる
- `binding`, `policy`, `attestations` のどこまでが plan/config の入力境界かを固定できる

## Implementation Slices

1. lock-to-plan compiler
2. lock-to-config compiler
3. binding / policy input threading
4. existing router integration
5. runtime guard adaptation
6. attestation non-input / advisory-input boundary の明文化

## Acceptance Criteria

- lock-first pathで execution plan を生成できる
- `config.json` が lock-derived で再生成できる
- 許可されていない ambient host state を plan 生成が暗黙に読まない
- Nacelle 連携が派生 IR 経由で維持される
- execution plan / `config.json` が attestation を必須入力とせず、必要時のみ明示境界から受け取る

## Primary Touchpoints

- execution plan 系
- router 系
- Nacelle 向け config 生成部
- runtime manager / guard

## Open Questions

- `contract.workloads` と既存 target/services model の最終マッピング
