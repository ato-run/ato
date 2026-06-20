# Ticket 04: Shared Source Compiler

- Status: Proposed
- Priority: P1
- Depends on: 01, 02, 03
- Blocks: 05, 06, 10

## Goal

`ato run`・`ato init`・source-backed `ato publish` が共通で使う source compiler を実装する。

## Scope

- Infer / Resolve / Materialize の 3 段分離
- deterministic evidence precedence 実装
- provenance 記録
- bounded observation / sandbox-assisted resolution の入口設計
- selection / confirmation / approval gate の分離
- publish 向け command-independent compile handoff の導入

## Out Of Scope

- registry upload / destination API の最終仕様
- registry API

## Required Outcomes

- run / init / source-backed publish が別々の heuristic 実装を持たない
- source-only input から canonical lock-shaped state を合成できる
- unresolved state を command semantics に応じて扱い分けられる
- equal-ranked candidate と script-capable resolution を fail-closed に制御できる
- publish が source input を lock-shaped compile output 経由で扱える

## Implementation Slices

1. engine skeleton
2. infer/process
3. infer/network
4. infer/env_contract
5. infer/filesystem
6. resolve/runtime
7. resolve/closure
8. decision gates for selection / approval
9. materialize/ephemeral
10. materialize/workspace
11. publish compile handoff

## Acceptance Criteria

- 同じ source input に対し run / init / publish が同じ compiler rules を使う
- `contract.process` の候補選定が deterministic precedence で動く
- weak evidence は durable contract へ自動昇格しない
- provenance から field の由来を inspect できる構造になる
- equal-ranked candidates が残る場合、run は explicit selection / confirmation か fail、init は unresolved marker か explicit selection へ進む
- script-capable resolution が explicit mode gate なしに durable output へ昇格しない
- approval / consent が通常の inference evidence と混線しない
- publish が source input を ad hoc manifest-first path ではなく canonical lock-shaped handoff から処理できる

## Primary Touchpoints

- 新規 `apps/ato-cli/src/application/source_inference/*`
- `apps/ato-cli/src/application/workspace/init/*`
- `apps/ato-cli/src/cli/commands/run.rs`
- `apps/ato-cli/src/application/pipeline/phases/publish.rs`

## Open Questions

- script-capable resolution を run でどこまで既定許容するか
- observation provenance を canonical lock 外のどこへ置くか
