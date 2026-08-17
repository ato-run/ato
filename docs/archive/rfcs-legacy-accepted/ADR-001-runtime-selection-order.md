---
title: "ADR-001: Runtime selection order & preference precedence"
status: accepted
date: 2026-01-29
author: "@egamikohsuke"
related: []
---

# ADR-001: Runtime selection order & preference precedence

## Context
Multi-target manifestsが増え、既定のランタイム選択順序と`execution.preference`の扱いが曖昧だった。
実装と仕様の不一致を解消し、明示ターゲット指定と優先順位の関係を固定する必要がある。

## Decision
- 既定の解決順序は `nacelle -> oci → wasm → source` とする。
- `execution.preference` は override 順序として強制適用する。
- 明示ターゲット指定（`execution.runtime` または単一の `targets.*`）は `execution.preference` より優先する。
- `execution.preference` が候補に一致しない場合は、候補内で既定順序にフォールバックする。

## Consequences
- ルーティング実装と仕様を更新する。
- 優先順位を設定する場合は、提供するターゲットに一致する順序を指定する必要がある。
