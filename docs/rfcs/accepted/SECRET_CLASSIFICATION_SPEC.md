---
title: "Secret Classification Spec"
status: accepted
date: "2026-02-23"
author: "@egamikohsuke"
ssot: []
related: []
---

# Secret Classification Spec

## 1. 目的

本書は secret の分類と注入経路を固定し、`env` 利用可否の解釈差を排除する。

## 2. 分類

### 2.1 user_secret

- 例: API key, database password, private token
- 取り扱い:
  - env 直接注入を禁止
  - `pipe(2)` / `memfd_create(2)` の FD で受け渡し
  - ログ・診断出力で値を常時マスク

### 2.2 session_token

- 例: `ATO_BRIDGE_TOKEN`, `CAPSULE_IPC_*` の短命セッショントークン
- 取り扱い:
  - allowlist 管理下で env 注入を許可
  - TTL を持つ短命トークンのみ許可
  - 権限は最小化（用途限定・スコープ限定）

## 3. 実装規約

- `ExecutionPlan.runtime.policy.secrets` は `user_secret` の許可集合を表す。
- `session_token` の注入可否は host 側（ato-cli / ato-desktop）が明示決定する。
- nacelle は分類の意思決定を行わず、渡された入力を隔離境界内で適用する。

## 4. 互換性

- 分類定義の追加・変更はセキュリティ互換性レビューを必須とする。
- `user_secret` の env 注入許可への変更は禁止（breaking ではなく非許容）。
