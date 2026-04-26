---
title: "Store Web Specification"
status: accepted
date: "2026-02-16"
author: "@egamikohsuke"
ssot:
  - "apps/ato-store-web/"
related:
  - "STORE_SPEC.md"
  - "ATO_CLI_SPEC.md"
---

# Store Web Specification

## 1. 目的

`apps/ato-store-web` は Ato Store の Web GUI を提供する。

- 公開カタログの閲覧
- `.capsule` 配布情報（trust）の可視化
- Publisher の Source 管理と CI token 管理

`ato-store` は API/検証責務に集中し、Web UI は本仕様で分離管理する。

---

## 2. 配置と技術

- App path: `/Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-store-web`
- Framework: Astro
- Rendering: `output: "static"`（Astro 5 仕様）
- Dynamic routes: `/capsules/[publisher]/[slug]` と `/github.com/[owner]/[repo]` は `prerender=false`。`/:owner/:repo`, `/capsules/[slug]`, `/p/[publisher]/[slug]`, `/publishers/[publisher]/[slug]` は legacy として `410 Gone` を返す。
- LP `/` と Store 一覧 `/store` は SEO と鮮度のため `prerender=false` で SSR 配信する
- Islands: React
- Host target: Cloudflare Pages
- E2E: Playwright

### 2.1 Shared Dock Packages

- Store Web は Dock 系 read-only UI の visual source of truth とする。
- 共通 UI/正規化/adapter は以下の package に分離する。
  - `/Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/packages/dock-domain`
  - `/Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/packages/dock-data`
  - `/Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/packages/dock-react`
- `apps/ato-store-web` はこれらを `file:` 依存として読み、Astro shell は route, SSR, SEO, auth/session, owner mutation のみを担当する。
- shared React は Astro import を持たず、`fetch` / `window.location` / Cloudflare server context を直接参照しない。
- owner 向けの visibility / access / delete などの書き込み操作は Store Web shell から action props で注入する。

---

## 3. 環境変数

| Name                     | Description                                     | Default                 |
| ------------------------ | ----------------------------------------------- | ----------------------- |
| `PUBLIC_STORE_URL`       | Store API base URL                              | `https://api.ato.run`   |
| `PUBLIC_STORE_NAME`      | UI表示名                                        | `Ato Store`             |
| `PUBLIC_ENV`             | 環境ラベル                                      | `production`            |
| `PUBLIC_DEFAULT_CHANNEL` | distribution 解決の既定チャネル                 | `stable`                |
| `PUBLIC_SITE_URL`        | canonical site URL                              | `https://ato.run` |
| `ADMIN_GITHUB_IDS`       | 管理者 GitHub numeric ID allowlist (CSV)        | `""`                    |
| `PLAYGROUND_ADMIN_TOKEN` | Store API admin token（Secret、ブラウザ非公開） | `""`                    |

`/install.sh` は以下の環境変数を解釈する（Web build env とは独立）:

- `ATO_RELEASE_BASE_URL`（default: `https://dl.ato.run`）
- `ATO_INSTALL_DIR`（default: `~/.local/bin`）
- `ATO_SKIP_CARGO_FALLBACK`（default: `0`）

---

## 4. ルーティング

### 4.1 Public

| Route                   | Rendering               | Purpose                          |
| ----------------------- | ----------------------- | -------------------------------- |
| `/`                     | SSR (`prerender=false`) | LP、Popular導線                  |
| `/api/og`               | Runtime API             | Dynamic OGP SVG (`path/slug/id`) |
| `/install.sh`           | Static Asset            | CLI インストーラスクリプト配信   |
| `/github.com/[owner]/[repo]` | SSR (`prerender=false`) | GitHub public repo 表示、GitHub import 導線 |
| `/s/[id]`               | SSR (`prerender=false`) | Workspace Share 詳細、Try/Decap 導線 |
| `/capsules`             | Static                  | 一覧/検索                        |
| `/capsules/[publisher]/[slug]` | SSR (`prerender=false`) | 詳細、distribution、download導線 |
| `/d/[handle]`           | SSR (`prerender=false`) | Personal Dock 公開ページ         |
| `/:owner/:repo`         | SSR (`prerender=false`) | legacy (`410 Gone`) |
| `/capsules/[slug]`      | SSR (`prerender=false`) | legacy (`410 Gone`) |
| `/publishers/[publisher]/[slug]` | SSR (`prerender=false`) | legacy (`410 Gone`) |
| `/p/[publisher]/[slug]` | SSR (`prerender=false`) | legacy (`410 Gone`) |

### 4.2 Publisher

| Route                  | Rendering       | Purpose                              |
| ---------------------- | --------------- | ------------------------------------ |
| `/dock`                | Static + Island | Dock library / owner capsule control |
| `/dock/capsules`       | Static + Island | Source一覧、同期                     |
| `/dock/capsules/new`   | Static + Island | App install/link、source登録         |
| `/dock/tokens`         | Static + Island | CI token CRUD                        |
| `/auth/callback`       | Static          | OAuth後の遷移受け口                  |
| `/github/callback`     | Static          | GitHub App Setup URL callback 受け口 |

### 4.3 Admin

| Route                 | Rendering               | Purpose                         |
| --------------------- | ----------------------- | ------------------------------- |
| `/admin`              | SSR (`prerender=false`) | `/admin/reviews` へリダイレクト |
| `/admin/reviews`      | SSR + Island            | Review Queue + Kill Switch      |
| `/admin/reviews/[id]` | SSR + Island            | Inspection & Decision           |

---

## 5. API 契約

使用 API は `STORE_SPEC` を正とする。Web 導線で必須なのは以下。

- Public: `/v1/capsules*`, `/v1/capsules/by/:publisher/:slug/vouches`, `/v1/capsules/by/:publisher/:slug/distributions`, `/v1/capsules/by/:publisher/:slug/download`
- Public: `/v1/github/repos/:owner/:repo`（GitHub public repo metadata + `capsule.toml` probe）
- Auth: `/api/auth/signin/github`, `/api/auth/session`
- Publisher: `/v1/publishers/me`, `/v1/sources*`, `/v1/publish/tokens*`
- Dock: `/v1/docks`, `/v1/docks/:handle`, `/v1/docks/:handle/items`, `/v1/docks/:handle/submission`, `/v1/docks/:handle/submit`, `/v1/uploads/presign`
- GitHub App UX: `/v1/sources/github/app/install-url`, `/v1/sources/github/app/callback`, `/v1/sources/github/app/installations`, `/v1/sources/github/app/installations/:installation_id/repositories`

Admin UI は browser から直接 Store API を呼ばず、同一オリジンの proxy を必須とする。

- `GET /api/admin/reviews`
- `GET /api/admin/reviews/:id`
- `POST /api/admin/reviews/:id/start-review`
- `POST /api/admin/reviews/:id/approve`
- `POST /api/admin/reviews/:id/reject`
- `POST /api/admin/reviews/:id/suspend`

Proxy 要件:

- Cookie を `/api/auth/session` へ転送してセッションを検証
- D1 `account` (`providerId='github'`) から GitHub numeric ID を解決し `ADMIN_GITHUB_IDS` と照合
- 非GET は `Origin` を必須検証
- Store API 呼び出し時のみ `PLAYGROUND_ADMIN_TOKEN` を付与
- 応答は `Cache-Control: private, no-store`

追加必須 API:

- `GET /v1/sources`
  - Publisher が保有する source 一覧
  - `latest_release_version` を含む

`GET /v1/capsules/by/:publisher/:slug` は以下の説明フィールドを返す前提で描画する:

- `description` (plain fallback)
- `description_markdown` (`string | null`)
- `description_markdown_source` (`string | null`)
- `description_render_format` (`markdown | plain`)
- `bug_tracker` (`string | null`)
- `vouches_count` (`number`)

### 5.1 Cache + Hydration 方針

- `/store` はサーバー側で `GET /v1/capsules` を実行し、SSR HTML を返す。
- `/store` の HTML レスポンスは `Cache-Control: public, s-maxage=60, stale-while-revalidate=600` を返す。
- React Island (`StoreFront`) は SSR 取得結果を `initialData` として受け取り、TanStack Query へ hydration する。
- TanStack Query は以下で固定する。
  - `queryKey: ["capsules", { q, category, limit }]`
  - `staleTime: 1000 * 60 * 5`
- Hydration 後の同一キー再取得は `staleTime` 期間内に抑制する。

### 5.2 LP (`/`) Popular / Tags / Categories

- `/` はサーバー側で `GET /v1/capsules?limit=8` を実行し、Popular データを SSR で描画する。
- `/` の HTML レスポンスは `Cache-Control: public, s-maxage=60, stale-while-revalidate=600` を返す。
- Popular は `downloads DESC` の上位 8 件を表示し、カード導線は `/capsules/:publisher/:slug` に統一する。
- Popular 取得失敗時は LP 全体を落とさず、空状態 (`No popular packages yet`) を表示する。
- LP の CLI セクションは `curl -fsSL https://ato.run/install.sh | sh` を提示し、`/install.sh` を公開配信する。
- `/github.com/:owner/:repo` は GitHub public repo を SSR で表示し、`capsule.toml` の有無と `ato run github.com/:owner/:repo` の primary CLI 導線を提示する。secondary CLI 導線は `ato decap github.com/:owner/:repo --into ./<repo>` とし、`ato install --from-gh-repo ...` は互換用途の補助導線に留める。
- `/github.com/:owner/:repo` は canonical を `https://ato.run/github.com/:owner/:repo`（staging: `https://staging.ato.run/github.com/:owner/:repo`）に固定する。
- `/s/:id` は share revision を表示し、上部の主 CTA を次の 2 つに固定する。
  - `Try now` → immutable `ato run <revision_url>` または `ato run <revision_url> --entry <primary>`
  - `Set up locally` → immutable `ato decap <revision_url> --into ./<root>`
- production canonical host は `https://ato.run` とし、share page も `https://ato.run/s/:id` を公開 surface にする。staging は `https://staging.store.ato.run/s/:id` を維持する。
- `/s/:id` の Overview には `entries[]` と env requirement を表示し、`Setup Guide` / `Spec` と分離する。
- `/:owner/:repo` は GitHub import legacy route として `410 Gone` を返し、`/github.com/:owner/:repo` への移行案内のみを出す。
- Root の 2-segment 名前空間は将来の公式 Capsule (`publisher/slug`) 用に予約し、GitHub import とは完全に分離する。
- `/install.sh` には `Content-Type: text/plain; charset=utf-8` と短TTLキャッシュを設定する。
- `/api/og` は `path/slug/id` を正規化し、解決済み capsule は長TTL (`s-maxage=86400`)・未解決は短TTL (`s-maxage=60`) で返す。
- 詳細ページ CTA は `Share` に加えて `Copy Embed` を提供し、`https://play.ato.run/embed/:slug` の iframe snippet を発行する。
- Categories は type 固定値 (All / Webpage / App / Web App / CLI / Agent Skills) を維持する。
- Trending Tags は初期実装では固定プレースホルダ表示とする。
- TODO: Tags 実データ化は `stats` テーブル + cron (`15分`) で実装し、導入時は `s-maxage=300, stale-while-revalidate=3600` の長TTLを適用する。

---

## 6. 認証モデル

- Web 自身はセッションを保持しない。
- Store の Better Auth cookie を利用する。
- Launchpad/Admin SSR guard は marker cookie ではなく `/api/auth/session` のサーバー検証を使う。
- Browser fetch は `credentials: "include"` を必須とする。
- Publisher画面は **Client guard**（AuthGate/PublisherGate）で制御する。
- 初期は DID 鍵生成を Web で扱わない（Publisher 登録は CLI/手動）。

---

## 7. UI 状態モデル

Publisher 系 UI は以下3状態を必須実装する。

- `anonymous`
- `logged_in_no_publisher`
- `publisher_ready`

データ描画は `ViewState<T>` で統一する。

- `loading`
- `empty`
- `error`
- `success`

Public 詳細ページの Vouch 表示は以下を適用する。

- `vouches_count = 0`: `New Release` / `Be the first to verify`
- `vouches_count = 1..9`: `Verified by early adopters`
- `vouches_count >= 10`: `10+ Verified Users`

`vouches_count` は単独スコアではなく Trust matrix の `Verified by Community` 行として併記する。

Dock UI（`/dock`）は library-first を基本とし、Three Gates は onboarding panel として縮退表示する。

- `CLI Gate`: copyable command (`ato publish --artifact ...`) を表示し、`ato login` 済みなら My Dock が既定ターゲットになること、official Store は `--registry https://api.ato.run` または `--ci` が必要なことを明示する
- `GitHub Gate`: `Manage Sources` / `Connect Repo` 導線
- `D&D Gate`: precheck + presign upload + ingest（progress + retry）
- `Premium Path`: Official Submission Checklist + status (`Queue / Changes Requested / Verified`)

Dock 公開ページ（`/d/[handle]`）は以下を表示する。

- Dock badge（`Personal Dock (Unverified)` または `Official Marketplace (Verified)`）
- submission status（`Not Submitted / Queue / Changes Requested / Verified`）
- capsule ごとの `visibility` / `trust_badge` / `latest_version`

---

## 8. E2E 要件

### 8.1 Public 導線

1. `/` 表示
2. `/` レスポンスヘッダーに `stale-while-revalidate=600` が含まれる
3. `/` Popular が「動的カードまたは空状態」を表示する
4. `/capsules` 一覧表示
5. `/capsules/[publisher]/[slug]` で distribution 情報表示
6. `download` 導線が遷移可能
7. 詳細ページで `New Release` / `Verified by early adopters` / `10+ Verified Users` のいずれかが表示される
8. `GET /install.sh` が 200 でシェルスクリプト本文（`#!/bin/sh`）を返す

### 8.2 Publisher 導線

1. `/dock` がログイン導線 or Dock library を表示
2. `/dock/capsules` が一覧または空状態を表示
3. `/dock/capsules/new` で wizard UI が操作可能
4. `/dock/tokens` で token UI が操作可能

### 8.3 Responsive

- 360 / 768 / 1120 の3幅で主要導線が崩れない

---

## 9. 非機能

- モバイル/デスクトップ両対応
- A11y: `main` ランドマーク、skip link、`:focus-visible` を提供
- CORS は Store 側 `ALLOWED_ORIGINS` に Web ドメイン（例: `https://store.ato.run`, `https://staging.store.ato.run`）を含む
- 配布関連の trust 情報 (`owner/signature/attestation/provenance/community`) を表示する
- `Report an Issue` / `Send Feedback` は主要CTAと分離し、サイドバーまたはフッターに配置する

---

## 10. 既知制約

- `publishers/register` の DID proof 作成は Web未対応（Phase 2以降）
- OAuth の callback URL は環境ごとに OAuth App 設定が必要

## 11. Dock Routing Finalization

- `/d/:handle` を正本 URL とする。
- `https://{handle}.ato.run` は Worker host rewrite による alias とする。
- canonical は常に `https://store.ato.run/d/:handle` を出力する。
- staging/prod の導入・検証・rollback は [`docs/ops/DOCK_ROUTING_RUNBOOK.md`](../ops/DOCK_ROUTING_RUNBOOK.md) に従う。
