# Ticket 06: Make ato init Durable And Lock-First

- Status: Proposed
- Priority: P1
- Depends on: 02, 03, 04
- Blocks: 07, 10, 11

## Goal

`ato init` を prompt-based manifest generator から、workspace-scoped durable materialization command へ移行する。

## Scope

- `ato init` の主出力を `ato.lock.json` へ変更
- partially resolved durable output のサポート
- workspace-local binding seed / provenance cache の初期化
- 旧 manifest scaffold path の縮退または legacy 化
- durable inspect / preview のための provenance export point 整備

## Out Of Scope

- remote acquisition UX 全体
- publish / registry

## Required Outcomes

- `ato init` が durable baseline として `ato.lock.json` を書ける
- unresolved state が inspectable marker として残る
- 旧来の prompt / recipe ロジックを main path から外せる
- later inspect / preview surface が再利用できる durable provenance 出口を持てる

## Implementation Slices

1. init contract rewrite
2. durable lock writer
3. workspace-local side state writer
4. partially resolved durable output の validator
5. legacy manifest scaffold path の分離
6. provenance cache / inspect handoff point の整備

## Acceptance Criteria

- local source に対し `ato.lock.json` を生成できる
- remote source acquisition 後の workspace にも同じ materializer を適用できる構造になる
- ambiguity が残る場合は unresolved marker を durable output に残せる
- `ato init` が単なる prompt generator ではなくなる
- fallback / observation / user-confirmed information が durable provenance cache から追跡可能になる
- workspace-local binding seed が repo-tracked canonical execution state と混線しない

## Primary Touchpoints

- `apps/ato-cli/src/application/workspace/init/mod.rs`
- `apps/ato-cli/src/application/workspace/init/detect.rs`
- `apps/ato-cli/src/application/workspace/init/prompt/*`
- 新規 materialization module

## Open Questions

- 旧 `capsule.toml` scaffold を別コマンドへ分離するか、legacy submode として残すか
