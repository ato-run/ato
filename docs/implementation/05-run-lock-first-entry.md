# Ticket 05: Make ato run Lock-First

- Status: Proposed
- Priority: P1
- Depends on: 02, 03, 04
- Blocks: 07, 08

## Goal

`ato run` を source-started でも lock-first にし、execute 前には必ず canonical lock-derived immutable input を持つようにする。desktop native-delivery では artifact-import path と source-derived path を分けて扱う。

## Scope

- run 入口を input resolver ベースへ変更
- source input から ephemeral lock state を合成
- consumer pipeline が lock-derived input を読むように移行
- preflight / verify / execute が manifest direct read に依存しないよう整理
- desktop artifact-import run の導入
- source-derived desktop run の導入順を固定

## Out Of Scope

- durable workspace materialization
- publish flow

## Required Outcomes

- source-only でも run 開始前に canonical lock-shaped input が合成される
- active execution semantics が ad hoc source heuristics を引きずらない
- unresolved security-sensitive field は execute 前に fail-closed する
- expected network/binding contract も immutable input の一部として確定する
- imported desktop artifacts が provenance-limited path として実行できる

## Implementation Slices

1. run command entry rewrite
2. attempt-scoped lock materialization
3. lock-derived pipeline request 追加
4. preflight の lock-first 化
5. verify / dry-run の lock-first 化
6. existing manifest direct paths の縮退
7. desktop artifact-import run path
8. source-derived desktop run handoff

## Acceptance Criteria

- `ato.lock.json` があればそれを読み、manifest の意味論を上書きしない
- source-only directory に対して ephemeral lock state を合成して run できる
- `binding` は host-local で materialize され、canonical hash に影響しない
- process/runtime/closure/security-sensitive field が unresolved の場合は execute に進まない
- expected network/binding contract が execute 前に固定される
- `.app` / `.AppImage` / `.exe` を artifact-import として run できるが、build reproducibility は claim しない
- source-derived desktop run の優先順位は Tauri -> Electron -> Wails で実装する

## Primary Touchpoints

- `apps/ato-cli/src/cli/commands/run.rs`
- `apps/ato-cli/src/cli/commands/run/preflight.rs`
- `apps/ato-cli/src/application/pipeline/phases/run.rs`
- execution plan / launch context 境界

## Open Questions

- background mode で ephemeral lock cache をどこまで再利用するか
