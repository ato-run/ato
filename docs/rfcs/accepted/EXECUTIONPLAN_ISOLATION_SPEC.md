---
title: "ExecutionPlan Isolation Spec"
status: accepted
date: "2026-02-24"
author: "@egamikohsuke"
ssot: []
related: []
---

# ExecutionPlan Isolation Spec

## 1. Scope

本書は ExecutionPlan の規範仕様（同意契約、fail-closed、互換性ルール）を定義する。
設計背景は `EXECUTIONPLAN_ISOLATION_MODEL.md` を参照する。

## 2. Tier Derivation

- `tier` は保存せず、`target.runtime + target.driver` から導出する。
- 導出規則:
  - `web/browser_static` → `tier1`
  - `source/deno` → `tier1`
  - `source/node` → `tier1`
  - `wasm/wasmtime` → `tier1`
  - `source/python` → `tier2`
  - `source/native` → `tier2`
- 導出不能または矛盾時は `ATO_ERR_POLICY_VIOLATION` で fail-closed。

## 2.1 Lock Requirements

- `source/deno` は `deno.lock` または `package-lock.json` を要求する。
- `source/node`（Tier1）は `package-lock.json` のみを要求する。
- `source/python` は `uv.lock` を要求する。
- Tier1 は追加で `capsule.lock.json` を必須とする。

## 3. Consent Contract

同意キーは以下の5要素で固定する。

- `scoped_id`
- `version`
- `target_label`
- `policy_segment_hash`
- `provisioning_policy_hash`

`policy_hash` / `runtime_policy_hash` などの別名は非許容。

## 4. Canonicalization

- 正規化と hash 算出は `EXECUTIONPLAN_CANONICALIZATION_SPEC.md` に従う。
- 変更は同意再取得を伴う breaking change とする。

## 5. Secret Handling

- secret 分類と注入経路は `SECRET_CLASSIFICATION_SPEC.md` に従う。
- `user_secret` の env 直接注入は禁止。

## 6. Diagnostics Boundary

- engine 内部I/O は engine spec に従う。
- 外部契約としての診断コード体系は `ATO_ERROR_CODES.md` を正本とする。

## 7. Canonical Handle Default Isolation

- `capsule://...` handle を起点にした未信頼 launch は isolation-first を既定とする。
- review modal は既定フローに入れない。
- 初期 isolation preset は fail-closed:
  - `network = false`
  - `filesystem_read = false`
  - `filesystem_write = false`
  - `secrets = false`
  - `devices = false`
- host は deny だけで終わらず、requestable capability については JIT prompt を上げられる。

## 8. Permission Request Contract

- JIT prompt は host が描画する secure overlay とする。
- v1 の action set:
  - `allow_once`
  - `allow_for_session`
  - `deny`
- trust decision と resolved metadata cache は責務を分離する。
  - metadata cache は `canonical_handle`, `snapshot`, `summary`, `fetched_at` を保持
  - local trust store は user/session decision を保持
