# Attestation-First Runbook（GitHub App権限有効化後）

最終更新: 2026-02-12

このドキュメントは、GitHub App 権限（`contents:write`, `pull_requests:write`）を有効化した後に必要な残手順を、実行順でまとめたものです。

## 0. 前提

- 対象リポジトリ:
  - `/Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-store`
- GitHub App はインストール済みで、publisher と linked 済み
- `apps/ato-store/.dev.vars` に必要値が設定済み

## 1. DB マイグレーション適用

### 1-1. Local

```bash
pnpm -C /Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-store db:migrate
```

### 1-2. Remote（staging/prod）

```bash
pnpm -C /Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-store db:migrate:remote
```

### 1-3. 確認

新規テーブル/カラムが反映されていることを確認する。

- `ci_publish_tokens`
- `build_attestations.certificate_subject`
- `build_attestations.certificate_issuer`
- `build_attestations.rekor_log_index`
- `build_attestations.integrated_time`
- `build_attestations.verified_at`

## 2. Store 設定値の確認

`apps/ato-store/.dev.vars`（および本番環境変数）で以下を確認する。

- `GITHUB_APP_ID`
- `GITHUB_APP_SLUG`
- `GITHUB_APP_PRIVATE_KEY`
- `GITHUB_ATTESTATION_ISSUER`（未設定ならデフォルト `https://token.actions.githubusercontent.com`）

## 3. CI Publish Token 発行

`POST /v1/publish/tokens` で publisher 単位の短TTLトークンを発行する。

```bash
curl -X POST http://localhost:8787/v1/publish/tokens \
  -H 'Authorization: Bearer <github_oauth_token>' \
  -H 'Content-Type: application/json' \
  -d '{"ttl_seconds":3600,"scopes":["publish:ci"]}'
```

期待レスポンス例:

```json
{
  "id": "01...",
  "token": "cipt_...",
  "scopes": ["publish:ci"],
  "expires_at": "2026-..."
}
```

### 3-1. GitHub Secrets 登録

対象リポジトリの Secrets に `CIPUBLISH_TOKEN`（または workflow で参照する名前）として登録する。

## 4. Workflow Bootstrap PR 作成

GitHub App 経由で workflow PR を自動生成する。

```bash
curl -X POST http://localhost:8787/v1/sources/<source_id>/bootstrap-workflow \
  -H 'Authorization: Bearer <github_oauth_token>'
```

期待レスポンス例:

```json
{
  "status": "pr_created",
  "pull_request_url": "https://github.com/.../pull/...",
  "workflow_path": ".github/workflows/capsule-build.yml"
}
```

### 4-1. PR レビュー観点

- `capsule-dev/capsule-build-action@v1` を参照している
- `build.lifecycle` を読む構成になっている
- `X-CI-Publish-Token` を使って Store に publish する設計になっている

## 5. CI Publish の E2E 実行

1. bootstrap PR を merge
2. release tag（例: `v1.0.0`）を push
3. GitHub Actions 実行を確認
4. Store 側で `/v1/publish/ci` が 201 を返していることを確認

## 6. 公開判定（Trust Gate）確認

`GET /v1/capsules/:id/distributions` の戻り値で以下を確認する。

- `owner_status = verified`
- `signature_status = verified`
- `attestation_status = verified`
- `provenance_status = full`
- `source_commit` が埋まっている
- `builder_identity` が埋まっている

```bash
curl 'http://localhost:8787/v1/capsules/<slug>/distributions?os=linux&arch=x86_64&channel=stable'
```

## 7. Attestation 詳細確認

```bash
curl http://localhost:8787/v1/attestations/<artifact_id>
```

確認項目:

- `verified: true`
- `certificate_issuer` が Fulcio 系
- `rekor_log_index` が入っている
- `verified_at` が記録されている

## 8. Token 運用

### 8-1. 一覧

```bash
curl http://localhost:8787/v1/publish/tokens \
  -H 'Authorization: Bearer <github_oauth_token>'
```

### 8-2. 失効

```bash
curl -X DELETE http://localhost:8787/v1/publish/tokens/<token_id> \
  -H 'Authorization: Bearer <github_oauth_token>'
```

運用ルール:

- TTL は短く（推奨 1h〜24h）
- リポジトリごとに token 分離
- ローテーション時は旧 token を即時失効

## 9. ブロック運用

不正配布/改ざん検知時は理由コード付きで source を block する。

```bash
curl -X POST http://localhost:8787/v1/sources/<source_id>/block \
  -H 'Authorization: Bearer <github_oauth_token>' \
  -H 'Content-Type: application/json' \
  -d '{"reason_code":"security_artifact_tamper","note":"digest mismatch"}'
```

## 10. ロールアウト順序（推奨）

1. dev で token + bootstrap + publish + distributions を通す
2. staging で同一手順を再現
3. publisher 単位 feature flag で `attestation_required` を有効化
4. production へ段階展開

## 11. 監視指標

最低限、以下を可視化する。

- `ci_publish_success_rate`
- `attestation_verify_fail_rate`
- `public_transition_rate`
- `rejection_reason_topN`

## 12. 受け入れチェック

- `POST /v1/publish/ci` が `X-CI-Publish-Token` なしで拒否される
- 同一 `idempotency_key` 再送で重複レコードが増えない
- `attestation NG / DID NG / hash NG` は `publish_events.status=rejected`
- Trust Gate 条件を満たす場合のみ `capsules.visibility=public`
