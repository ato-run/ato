---
title: "Schema Registry & Versioning"
status: accepted
date: "2026-01-29"
author: "@egamikohsuke"
ssot: []
related:
  - "TRUST_AND_KEYS.md"
  - "MAG_URI.md"
---

# Schema Registry & Versioning (Draft)

## 1. 目的
- グローバルな Schema ID の共有
- Polymorphism の基盤

## 2. Schema ID
### 2.1 Canonical Hash
- Schemaの実体は **正規化されたスキーマ定義のハッシュ**を正とする
- `SchemaHash = sha256(canonical(schema))`
- canonical は **JCS (RFC 8785)** を正とする

### 2.2 Human Alias
- `std.todo.v1` などの**人間可読な別名**を許可
- 別名は **Registry で `SchemaHash` にマップ**される
- 実行時は **Hash を正** とする

## 3. 互換性
### 3.1 バージョニング
- 互換性を破壊する場合は **新しい SchemaHash** を生成する
- `v1`, `v2` などの表記は **エイリアス層**として扱う

### 3.2 マイグレーション
- 変換ロジックは **Capsule 側**が担う
- Registry は「推奨マイグレーター」の参照情報を持てる

## 4. 解決方式
### 4.1 Resolution Flow
1. ローカルキャッシュに `SchemaHash` があればそれを使用
2. なければ Registry で別名を解決
3. 取得結果はローカルにキャッシュ

### 4.2 Local Registry File
- `~/.capsule/schema_registry.json` を参照する
- 形式は `aliases: { alias: "sha256:..." }` の単純マップ

### 4.2 Registry Record（例）
```json
{
	"alias": "std.todo.v1",
	"schema_hash": "sha256:...",
	"maintainers": ["did:key:..."],
	"migrators": ["did:key:.../capsule"],
	"created_at": "2026-01-23T00:00:00Z"
}
```

## 5. 未決事項
### 5.1 ガバナンス
- Registry は **署名付きの append-only** を基本とする
- 署名主体は `maintainers` の DID

### 5.2 信頼モデル
- 署名検証は必須
- `maintainers` の信頼関係は `TRUST_AND_KEYS` に準拠

### 5.3 取得経路
- Domain Anchor からのHTTP取得
- P2P配布（MagNet）
