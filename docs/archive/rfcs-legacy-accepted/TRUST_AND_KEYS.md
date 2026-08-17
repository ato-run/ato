---
title: "Trust UX & Key Management"
status: accepted
date: "2026-01-29"
author: "@egamikohsuke"
ssot: []
related:
  - "IDENTITY_SPEC.md"
  - "SIGNATURE_SPEC.md"
---

# Trust UX & Key Management (Draft)

## 1. 目的
- 署名/検証の可視化
- 鍵の失効/ローテーション

## 1.1 脅威モデル（簡易）
- **偽Capsule配布**: 署名検証で拒否
- **鍵漏洩**: 失効リストとローテーションで対処
- **中間者**: TOFUで初回指紋を固定

## 2. Trust UX
- TOFU
- Petnames
- Badge 表示

### 2.1 TOFU (Trust On First Use)
- 初回接続時に Fingerprint を記録
- 再接続時に差異があれば警告
- 保存先: `~/.capsule/trust_store.json`

### 2.2 Petnames
- ユーザーが任意名を付与
- 保存先はローカルキーストア
- 保存先: `~/.capsule/petnames.json`

### 2.3 Badge
- 検証済み署名にバッジ表示

### 2.4 Trust State
- **Verified**: 署名一致 + 失効なし
- **Untrusted**: 署名不一致 / 失効済み
- **Unknown**: 未検証（ネットワーク未到達）

### 2.5 UX フロー（最小）
1. 初回取得時に Fingerprint を保存
2. 以後の一致/不一致を表示
3. 不一致時は強制確認

## 3. Key Management
- 失効リスト
- ローテーション

### 3.0 鍵のスコープ
- **Capsule署名鍵**: 配布物の署名
- **User/Device鍵**: P2P通信や個人所有の証明

### 3.1 失効リスト
- `revocation.json` を署名付きで配布
- Runtime はローカルにキャッシュ

**最小形式**
```json
{
	"version": "1",
	"issued_at": "2026-01-29T00:00:00Z",
	"revoked_keys": [
		{ "key_id": "did:key:...#signing", "revoked_at": "2026-01-29T00:00:00Z", "reason": "compromised" }
	],
	"signature": "base64..."
}
```

**配布経路（最小）**
- DNS/TXT or HTTP で取得（Domain Anchor）
- P2P 配布は任意

### 3.2 ローテーション
- 新旧鍵の併用期間を許可
- `signature.json` に `previous_key` を追加可能

**推奨最大併用期間**
- 30日以内

**推奨手順**
1. 新鍵で署名開始
2. `previous_key` を併記
3. グレース期間経過後に旧鍵を失効

### 3.3 鍵の保管
- OS Keychain / Secure Enclave を優先
- 平文ファイル保存は禁止

### 3.4 Trust UX（運用）
- 署名不一致時は「一時的な許可」ではなく**再検証**が必須
- Petname は Trust Store のキーIDと紐付ける

## 4. 未決事項
- Recovery UX
- 監査ログ

### 4.1 Recovery UX
- Social Recovery の導入可否
- Custodial モードとの折衷

### 4.2 監査ログ
- 署名検証イベントをローカルに記録
- Exfiltrationは禁止（デフォルト）
