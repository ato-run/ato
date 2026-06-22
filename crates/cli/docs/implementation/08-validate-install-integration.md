# Ticket 08: Validate And Install Lock-First Integration

- Status: Proposed
- Priority: P2
- Depends on: 02, 05, 07, 11
- Blocks: 09

## Goal

`ato validate` と `ato install` を canonical input resolver 経由へ移し、lock-first semantics に合わせる。

## Scope

- validate の lock-first 化
- install の lock-first 化
- compatibility input との差分 diagnostics
- strict feature validation の command surface 反映
- workspace-local binding / policy state との整合

## Out Of Scope

- build / publish migration

## Required Outcomes

- validate が `ato.lock.json` を第一級入力として扱う
- install が lock-derived runtime / closure data を利用できる
- compatibility input は import source としてのみ参照される
- install が binding / policy precedence に従って materialization できる

## Implementation Slices

1. validate command rewrite
2. install command resolver integration
3. lock-first diagnostics
4. strict mode の unsupported feature handling
5. install の binding / policy state integration

## Acceptance Criteria

- `ato.lock.json` がある workspace で validate/install が manifest canonicality を前提にしない
- unresolved execution-critical state を validate が fail-closed に報告できる
- compatibility drift を advisory diagnostics として表示できる
- install が embedded binding を既定 authoritative source として扱わない

## Primary Touchpoints

- `apps/ato-cli/src/cli/commands/validate.rs`
- install command entry / application install flow
- runtime manager

## Open Questions

- install の output layout に embedded binding をどこまで含めるか
