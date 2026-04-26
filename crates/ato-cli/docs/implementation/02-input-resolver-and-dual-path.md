# Ticket 02: Input Resolver And Dual-Path Boundary

- Status: Proposed
- Priority: P0
- Depends on: 01
- Blocks: 05, 06, 08, 09

## Goal

canonical input の優先順位を一箇所に集約し、`ato.lock.json` / compatibility input / bootstrap の解決を共通化する。

## Scope

- input resolver の新設
- `ato.lock.json` 優先の解決順序実装
- compatibility input を import source として扱うポリシー固定
- diagnostics 用の input provenance 返却

## Out Of Scope

- source inference そのもの
- durable `ato.lock.json` 生成

## Required Outcomes

- 実行系コマンドが manifest を直接 authoritative input として読まなくなる入口を作る
- `ato.lock.json` と `capsule.toml` が同居したときに前者を authoritative とする
- compatibility input の差分を diagnostics に添付できる土台を作る

## Implementation Slices

1. `ResolvedInput` モデル追加
   - canonical lock
   - compatibility manifest
   - compatibility lock
   - source-only
2. 共通 resolver API 追加
3. run / init / validate / install の入口で resolver を呼ぶ準備
4. provenance と warning surface を設計
5. 同居時の authoritative / advisory ルールを実装

## Acceptance Criteria

- `ato.lock.json` が存在する場合、resolver は manifest-first path を返さない
- `capsule.toml` のみ存在する場合、resolver は compatibility input を返す
- 何も無い場合、resolver は bootstrap/source-only を返す
- CLI 入口からの direct manifest read が減り、resolver 経由へ集約される

## Primary Touchpoints

- 新規 `apps/ato-cli/src/application/input_resolver/*`
- `apps/ato-cli/src/cli/commands/run.rs`
- `apps/ato-cli/src/cli/commands/validate.rs`
- install / publish / build の入口

## Open Questions

- `ato.lock.json` と compatibility input 差分の表示を warn で止めるか inspect path へ回すか
