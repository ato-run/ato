# ATO Error Codes 仕様

**Status:** Draft  
**Last Updated:** 2026-02-23

## 1. 目的

本書は Ato の診断コード体系における単一の正規仕様（Source of Truth）を定義する。
`EXECUTIONPLAN_ISOLATION_SPEC.md` は出力契約境界（外部I/O）を定義し、
本書はコード命名・分類・互換性ポリシーを定義する。

## 2. 出力契約

- 媒体: stderr JSONL（1行1イベント）
- 最低必須フィールド:
  - `level`: `fatal` | `error` | `warn` | `info`
  - `code`: `ATO_ERR_*`
  - `message`: 人間可読の短文
- 推奨フィールド:
  - `resource`: 例 `network`, `filesystem`, `sandbox`, `lockfile`
  - `target`: 例 `api.evil.com`, `/tmp/foo`
  - `hint`: 復旧ヒント

例:

```json
{
  "level": "fatal",
  "code": "ATO_ERR_POLICY_VIOLATION",
  "message": "Network access denied",
  "resource": "network",
  "target": "api.evil.com"
}
```

## 3. 命名規約

- 形式: `ATO_ERR_<DOMAIN>_<DETAIL>`
- DOMAIN は以下のいずれかを使用する:
  - `POLICY`
  - `PROVISIONING`
  - `STORAGE`
  - `COMPAT`
  - `LOCKFILE`
  - `SANDBOX`
  - `INTERNAL`

## 4. 互換性ポリシー

- 既存コードの削除・意味変更はメジャー更新でのみ許可。
- 同義語の新設は禁止（1事象1コード）。
- 新規コード追加時は本書に追記した時点で有効。

## 5. 初期コード一覧

- `ATO_ERR_POLICY_VIOLATION`
- `ATO_ERR_PROVISIONING_LOCK_INCOMPLETE`
- `ATO_ERR_PROVISIONING_TLS_TRUST`
- `ATO_ERR_STORAGE_NO_SPACE`
- `ATO_ERR_COMPAT_HARDWARE`
- `ATO_ERR_LOCKFILE_TAMPERED`

## 6. 運用ルール

- 実装は本書のコードをそのまま出力すること。
- ドキュメント内サンプルのコードは本書を優先する。
