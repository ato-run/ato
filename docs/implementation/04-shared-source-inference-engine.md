# Ticket 04: Shared Source Inference Engine

- Status: Proposed
- Priority: P1
- Depends on: 01, 02, 03
- Blocks: 05, 06, 10

## Goal

`ato run` と `ato init` が共通で使う source inference / resolve / materialize pipeline を実装する。

## Scope

- Infer / Resolve / Materialize の 3 段分離
- deterministic evidence precedence 実装
- provenance 記録
- bounded observation / sandbox-assisted resolution の入口設計
- selection / confirmation / approval gate の分離

## Out Of Scope

- full publish flow
- registry API

## Required Outcomes

- run と init が別々の heuristic 実装を持たない
- source-only input から canonical lock-shaped state を合成できる
- unresolved state を command semantics に応じて扱い分けられる
- equal-ranked candidate と script-capable resolution を fail-closed に制御できる

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

## Acceptance Criteria

- 同じ source input に対し run と init が同じ inference rules を使う
- `contract.process` の候補選定が deterministic precedence で動く
- weak evidence は durable contract へ自動昇格しない
- provenance から field の由来を inspect できる構造になる
- equal-ranked candidates が残る場合、run は explicit selection / confirmation か fail、init は unresolved marker か explicit selection へ進む
- script-capable resolution が explicit mode gate なしに durable output へ昇格しない
- approval / consent が通常の inference evidence と混線しない

## Primary Touchpoints

- 新規 `apps/ato-cli/src/application/source_inference/*`
- `apps/ato-cli/src/application/workspace/init/*`
- `apps/ato-cli/src/cli/commands/run.rs`

## Open Questions

- script-capable resolution を run でどこまで既定許容するか
- observation provenance を canonical lock 外のどこへ置くか
