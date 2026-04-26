# Store Web Staging Runbook

`apps/ato-store-web` を staging で検証するための手順。

## 1. 前提

- Store API staging がデプロイ済み
  - `https://staging.api.ato.run`
- Web staging domain
  - `https://staging.store.ato.run`
- Store の `ALLOWED_ORIGINS` に `https://staging.store.ato.run` を含む
- GitHub App (staging) の Setup URL が `https://staging.store.ato.run/github/callback` に設定済み

## 2. ローカル実行

```bash
cd /Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-store-web
pnpm install
cp .env.example .env
cat <<'EOF' >> .env
PUBLIC_STORE_URL=https://staging.api.ato.run
PUBLIC_STORE_NAME=Ato Store Staging
PUBLIC_ENV=staging
PUBLIC_SITE_URL=https://staging.store.ato.run
EOF
pnpm dev
```

## 3. ビルド確認

```bash
pnpm build:staging
pnpm preview
```

注: `@astrojs/cloudflare` adapter では `astro preview` は非対応のため、`pnpm preview` は `wrangler pages dev ./dist` を利用する。

## 4. Pages 配備（staging）

```bash
pnpm deploy:staging
```

デプロイ前に `dist` へ埋め込まれた API endpoint を確認:

```bash
rg -n "API endpoint" dist/index.html
```

`https://staging.api.ato.run` になっていること。

## 5. 手動確認チェック

### Public

1. `/`
2. `/capsules`
3. `/capsules/<slug>`
4. `/p/<publisher>/<slug>`

確認ポイント:
- Trust matrix が 4軸表示される
- 配布候補なし時に理由と次アクションが表示される
- `description_markdown` がある capsule は Markdown 描画される
- `metadata.store_description` 未指定時は `README.md` が説明として表示される
- Markdown が取得できない capsule は `description` の plain 表示へフォールバックする

### Publisher

1. `/publish`
2. `/publish/sources`
3. `/publish/sources/new`
4. `/publish/tokens`

確認ポイント:
- 未ログイン時: Authentication Required のみ表示（生APIエラー非表示）
- Publisher未登録時: Registration Required のみ表示
- Source wizard が Step 1→2→3 で進行（`installation_id` 手入力なし）
- 「GitHubと連携する」→ GitHub 画面 → `/github/callback` 経由で `/publish/sources/new?linked=1...` に戻る
- repository は URL 手入力ではなく選択UIで登録できる
- Token revoke 時に確認ダイアログ表示

## 6. Playwright E2E

```bash
cd /Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-store-web
PLAYWRIGHT_BASE_URL=https://staging.store.ato.run pnpm test:e2e
```

distribution 固定ページも検証する場合:

```bash
PLAYWRIGHT_BASE_URL=https://staging.store.ato.run \
E2E_SAMPLE_PUBLISHER=koh0920 \
E2E_SAMPLE_SLUG=astro-blog \
pnpm test:e2e
```

Playground 導線（Store詳細 -> Theater -> sandbox iframe）も検証する場合:

```bash
PLAYWRIGHT_BASE_URL=https://staging.store.ato.run \
E2E_PLAYGROUND_SLUG=astro-blog \
E2E_PLAY_BASE_URL=https://staging.play.ato.run \
E2E_PLAY_SANDBOX_BASE_DOMAIN=staging.atousercontent.com \
pnpm test:e2e
```

`apps/ato-store-web/scripts/smoke_staging_web.sh` でも同等実行が可能:

```bash
cd /Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-store-web
PLAYGROUND_SLUG=astro-blog ./scripts/smoke_staging_web.sh
```

`PLAYGROUND_SLUG` を必須として厳密に検証する場合:

```bash
REQUIRE_PLAYGROUND_SLUG=1 PLAYGROUND_SLUG=astro-blog ./scripts/smoke_staging_web.sh
```

OG endpoint を失敗時も必ず赤にしたい場合:

```bash
STRICT_OG=1 ./scripts/smoke_staging_web.sh
```

sitemap に capsule URL が最低1件あることまで必須化する場合:

```bash
STRICT_SITEMAP=1 ./scripts/smoke_staging_web.sh
```

Hero animation (`data-testid="hero-three-animation"`) を必須化する場合:

```bash
STRICT_HERO=1 ./scripts/smoke_staging_web.sh
```

## 7. APIスモーク併走

`ato-store` 側も同時に実行する。

```bash
cd /Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-store
./scripts/smoke_staging_deploy.sh
```

## 8. トラブルシュート

- `auth_required`
  - Store session cookie が無い/期限切れ
- `publisher_required`
  - `POST /v1/publishers/register` が未実施
- `redirect_uri is not associated`
  - GitHub OAuth App callback URL 不一致
- CORS エラー
  - `ALLOWED_ORIGINS` に Web domain が不足
