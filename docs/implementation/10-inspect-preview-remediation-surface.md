# Ticket 10: Inspect / Preview / Remediation Surface

- Status: Proposed
- Priority: P2
- Depends on: 01, 02, 03, 04, 06, 07
- Blocks: none

## Goal

lock-first 移行後の利用者向け surface を整備し、`inspect`, `preview`, `diagnostics`, `remediation` が lock path と provenance を前提に一貫して動作するようにする。

## Scope

- inspect surface の lock-first 化
- preview surface の lock-first 化
- remediation suggestion の lock path 化
- diagnostics から inspect / preview へ遷移できる導線整備

## Out Of Scope

- secrets / identity approval UX の最終仕様
- organization policy authoring UI
- registry web UX

## Required Outcomes

- provenance が存在するだけでなく、利用者が field ごとの由来を参照できる
- partially resolved / unresolved state を lock path ベースで確認できる
- fallback, observation, user confirmation, approval gate の関与有無を surface できる
- remediation suggestion が manifest-first path ではなく lock path を主に返す

## Implementation Slices

1. inspect data model 定義
2. lock path + provenance renderer 実装
3. preview/import/init/run write-back preview 設計
4. diagnostics から inspect / preview への参照整備
5. remediation suggestion と source mapping surface 整備

## Acceptance Criteria

- inspect で field ごとに explicit / inferred / resolved / observed / user-confirmed を判別できる
- inspect で fallback 使用有無と security approval / consent gate の関与有無を確認できる
- unresolved marker が lock path と reason class 付きで表示される
- preview が durable `ato.lock.json` または ephemeral lock materialization の要点を確認できる
- diagnostics が lock path を主に返し、必要に応じて import source mapping を補助的に示せる

## Primary Touchpoints

- inspect / preview / diagnostics command surface
- provenance cache / inspection model
- init / run preview path
- remediation / diagnostics adapters

## Open Questions

- preview を専用コマンドに寄せるか既存 command の `--preview` に寄せるか
- remediation suggestion を machine-readable payload と human-readable text のどこまで二重化するか