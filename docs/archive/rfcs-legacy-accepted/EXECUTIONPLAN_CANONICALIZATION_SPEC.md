---
title: "ExecutionPlan Canonicalization Spec"
status: accepted
date: "2026-02-23"
author: "@egamikohsuke"
ssot: []
related: []
---

# ExecutionPlan Canonicalization Spec

## 1. 目的

本書は `ExecutionPlan` の canonical hash 生成規則を固定し、実装差による同意判定の非決定化を防止する。

## 2. 適用範囲

- `consent.policy_segment_hash`
- `consent.provisioning_policy_hash`

## 3. 正規化方式

- JSON canonicalization は RFC 8785 (JCS) に準拠する。
- hash アルゴリズムは BLAKE3 とし、表記は `blake3:<hex>`。

## 4. 入力境界

### 4.1 policy_segment_hash

入力対象は runtime 実効権限のみ。

- 含める: `runtime.policy`, `runtime.fail_closed`, `runtime.non_interactive_behavior`,
  runtime 実行時に自動導出される mount-set 生成ロジック hash
- 含めない: 表示名、説明、UX文言、ログメッセージ

### 4.3 mount-set 生成アルゴリズム契約

- `mount_set_algo_id`: アルゴリズム識別子（例: `lockfile_mountset_v1`）
- `mount_set_algo_version`: 互換性バージョン（整数）
- 上記2項目は `policy_segment_hash` の入力に必ず含める。
- `mount_set_algo_version` の更新は同意再取得を伴う breaking change とする。

### 4.2 provisioning_policy_hash

入力対象は provisioning 実効権限のみ。

- 含める: `provisioning.network`, `lock_required`, `integrity_required`, registry 制約
- 含めない: 実行時UI文言、非機能メタデータ

## 5. 配列・順序規則

- セマンティクス上順序非依存の配列（host allowlist、path集合等）は昇順ソートしてから hash 入力に含める。
- セマンティクス上順序依存の配列（起動引数など）は元順序を保持する。

## 6. path canonicalization 規則

- 相対パスは評価時の project root を基準に絶対化する。
- `.` / `..` を解決し、symlink は評価時点で実体解決した canonical path を使う。
- 大文字小文字差は OS 仕様に従う（macOS/APFS の case-insensitive 設定では比較前に正規化）。

## 7. 互換性ポリシー

- 本規則の変更は同意再取得を伴う breaking change として扱う。
- 変更時は `schema_version` を更新し、移行手順を明記する。
