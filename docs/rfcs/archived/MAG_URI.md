---
title: "mag:// URI Specification"
status: accepted
date: "2026-01-23"
author: "@egamikohsuke"
ssot: []
related:
  - "SCHEMA_REGISTRY.md"
  - "IDENTITY_SPEC.md"
---

# mag:// URI Specification (Draft)

## 1. 目的
- Locationではなく State/Schema を指す

## 2. 構文
`mag://<DID>:<SchemaHash>/<MerkleRoot>/<Path>`

### 2.1 要素定義
- **DID**: 発行者/所有者の識別子
- **SchemaHash**: スキーマの正規化ハッシュ
- **MerkleRoot**: 状態スナップショットの指紋
- **Path**: データ内パス（任意）

## 3. 解決フロー
1. **DID解決**（ローカルキャッシュ優先）
2. **Schema検証**（Schema Registry 参照）
3. **Capsule選択**（`implements` の一致）
4. **データ取得**（Local/Peer/Relay）

## 4. Domain Anchor
### 4.1 DNS/TXT
- `mag://<domain>/<path>` は DNS TXT を参照し、DIDへ解決する
- DNSは**Human Alias**として扱う（最終的にはDIDへ解決）

### 4.2 Local Domain Anchors
- `~/.capsule/domain_anchors.json` によるローカル解決を許可
- DNSが未設定の場合のフォールバックとして使用

## 5. 未決事項
### 5.1 キャッシュ期限
- DID/Schema解決結果のTTL

### 5.2 エラーフォールバック
- HTTPゲートウェイへのフォールバック可否
- P2P純血主義との整合
