# Store Web UI Wording Table

Ato Store Web の UI 文言を統一するための基準。

## 1. 認証・権限

| Key | Japanese |
| --- | --- |
| `auth_required.title` | Authentication Required |
| `auth_required.body` | セッションが無効か未ログインです。GitHub でログインして再試行してください。 |
| `publisher_required.title` | Publisher Registration Required |
| `publisher_required.body` | この操作には Publisher 登録が必要です。登録後にもう一度実行してください。 |

## 2. 状態表示

| Key | Japanese |
| --- | --- |
| `loading.default` | 読み込み中です... |
| `empty.sources` | 登録済み source はありません |
| `empty.tokens` | トークンは未作成です |
| `error.server` | サーバー側でエラーが発生しました。時間をおいて再試行してください。 |
| `error.unexpected` | 予期しないエラーが発生しました。 |

## 3. Trust 表示

| Key | Japanese |
| --- | --- |
| `trust.owner` | Owner Verification |
| `trust.signature` | Signature Status |
| `trust.attestation` | Attestation Status |
| `trust.provenance` | Provenance Status |
| `trust.missing` | このプラットフォーム向けの配布候補が見つかりません。 |

## 4. 操作文言

| Key | Japanese |
| --- | --- |
| `action.signin_github` | GitHub でログイン |
| `action.retry` | 再試行 |
| `action.copy` | Copy |
| `action.revoke` | Revoke |
| `action.register_source` | Register |

## 5. ガイドライン

1. エラー文言は `何が起きたか + 次アクション` を必ず含める。
2. 生のHTTPメッセージは表示しない。
3. 日英混在を避ける。英語ラベルを使う場合は全体で統一する。
