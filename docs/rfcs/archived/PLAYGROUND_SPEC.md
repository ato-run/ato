---
title: "Ato Playground Specification (v0.1)"
status: accepted
date: "2026-02-20"
author: "@egamikohsuke"
ssot: []
related:
  - "STORE_SPEC.md"
  - "STORE_WEB_SPEC.md"
---

# Ato Playground Specification

## 1. 目的

- `play.ato.run` を Theater UI として提供する。
- `*.atousercontent.com` で untrusted artifact を分離配信する。
- Publisher 申請 -> Admin 承認の半自動運用で配備を管理する。

---

## 2. ドメイン分離

- Trusted Origin: `https://play.ato.run`
- Untrusted Origin: `https://{subdomain}.atousercontent.com`
- Trusted から Untrusted は iframe 経由でのみ接続する。
- Untrusted は親 Origin の Cookie / LocalStorage にアクセスできない。

---

## 3. コンポーネント責務

### 3.1 Control Plane (`apps/ato-store`)

- `POST /v1/playground/deployments` で deployment を作成し、自動ゲートを実行する。
- `POST /v1/playground/deployments/:id/submit` で Draft を審査申請する。
- `POST /v1/playground/deployments/:id/start-review` で Human Review を開始する。
- `POST /v1/playground/deployments/:id/approve` で公開承認する。
- `POST /v1/playground/deployments/:id/reject` でリジェクトする（カテゴリ必須）。
- `POST /v1/playground/deployments/:id/suspend` で停止する。
- `GET /v1/capsules/:id/playground` で Theater 用実行情報を返す。

### 3.2 Data Plane (`apps/ato-play-edge`)

- Host から subdomain を抽出する。
- `PLAYGROUND_KV` で manifest を解決する。
- `ARTIFACTS_BUCKET` から object を配信する。
- 動的 CSP / COOP / CORP / Referrer ヘッダーを注入する。

### 3.3 Theater UI (`apps/ato-play-web`)

- `/` で Launchpad を表示する。
- `/:slug` と `/?id={slug}` で Theater を表示する。
- `/embed/:slug` で Embed 専用 Theater (Read-Only) を表示する。
- Launchpad は `Continue Building` / `Made for You` / `Fresh & Trending` の 3 行で表示する。
- Store API で iframe URL を解決し、Sandbox iframe を描画する。
- Store API で `GET /v1/capsules?filter=playground&rank=engaged&limit=20` を取得し、推薦行の元データにする。
- `postMessage` bridge を `origin` 検証付きで受信する。
- Embed モードは Click-to-Load を必須とし、初回クリックまで iframe/sandbox を起動しない。
- Embed モードは Read-Only とし、vouch/purchase/sidebar 等の状態変更導線を表示しない。

### 3.4 Launchpad 推薦ポリシー（MVP v1）

- `Continue Building`:
  - `localStorage` の `ato.play.recent_slugs` を LRU で管理（最大20件）。
  - 直近利用順で最大6件を表示する。
- `Made for You`:
  - 直近利用 Capsule をアンカーに、`category/type/playground_target/publisher` 類似で最大4件を表示する。
  - `Continue Building` と重複させない。
- `Fresh & Trending`:
  - 10件中8件は engaged 上位、2件は探索枠を混在させる。
  - 探索候補は「新着14日以内」または「低露出（downloads 下位帯）」を優先する。
  - 探索枠は `YYYY-MM-DD + ato.play.anon_id` を seed にして日次で安定化する。
  - `Continue Building` / `Made for You` と重複させない。
- 最低限の Launchpad 計測:
  - クリック時に `row_type`, `position`, `slug`, `clicked` をクライアントイベントとして記録する。

---

## 4. 配備ワークフロー (Phase 1.1)

1. Publisher が `POST /v1/playground/deployments` を実行。
2. Store は自動ゲート（build/smoke）を実行し、`gate_status` と `gate_report_json` を保存する。
3. ゲート通過時のみ `review_status=draft` を付与し、`submit` 可能状態にする。
4. Publisher が `POST /v1/playground/deployments/:id/submit` を実行し、`review_status=submitted` へ遷移。
5. AI 一次審査結果は記録のみ行い、公開判定には使わない（Phase 1）。
6. Admin が `POST /v1/playground/deployments/:id/start-review` で `in_review` に遷移。
7. Admin が `approve` または `reject` を実行。
8. `approve` 時のみ `capsules.playground_status=approved` として KV に manifest を upsert。
9. Admin が `suspend` を実行すると、KV を tombstone 化し配信停止。
10. `suspend` は `capsules.visibility=blocked` も同時更新し、Store download を遮断する。

### 4.1 レビューステータス

- `draft | submitted | in_review | approved | rejected | suspended`
- 互換のため `playground_deployments.status` は継続するが、意味は以下にマップする:
  - `approved -> approved`
  - `suspended -> suspended`
  - その他レビュー状態 -> `requested`

### 4.2 ゲートレポート契約

`gate_report_json` は以下を保持する。

```json
{
  "checks": [
    { "name": "build", "status": "passed|failed", "duration_ms": 1234 },
    { "name": "smoke", "status": "passed|failed", "duration_ms": 3456 }
  ],
  "error_type": "system|application|null",
  "error_code": "G001|null",
  "error_message": "..."
}
```

- `error_type=system` は最大3回の指数バックオフ再試行対象。
- `error_type=application` は再試行せず開発者修正待ち。

### 4.3 ソース更新時の審査リセット

- `submitted` または `in_review` 中に `source_revision` が変更された場合、当該 deployment は `draft` に戻る。
- リセット時は `gate_status=queued`、`submitted_revision` をクリアする。

### 4.4 楽観ロック（expected_status）

- `approve/reject/suspend` は optional body `expected_status` を受け取る。
- 更新条件は `WHERE id = ? AND review_status = expected_status_or_required_state`。
- 条件不一致で更新件数が 0 の場合は `409 review_conflict` を返す。
- 既存互換のため `expected_status` 未指定時は従来必須状態で判定する。

---

## 5. KV 契約

- Key: `playground:v1:subdomain:{label}`
- Value(JSON):

```json
{
  "capsule_id": "01HQ...",
  "slug": "my-capsule",
  "release_id": "01HR...",
  "subdomain": "my-capsule-x9z",
  "artifact_root": "playground/01HQ.../1.2.0",
  "entry_path": "index.html",
  "target": "static",
  "permissions": {
    "connect_allowlist": ["https://api.example.com"],
    "capabilities": []
  },
  "status": "approved",
  "updated_at": "2026-02-15T00:00:00Z"
}
```

停止時 tombstone:

```json
{
  "capsule_id": "01HQ...",
  "slug": "my-capsule",
  "status": "suspended",
  "tombstone": true,
  "suspended_at": "2026-02-15T00:00:00Z",
  "updated_at": "2026-02-15T00:00:00Z"
}
```

---

## 6. セキュリティ要件

- CSP は Worker で動的生成する。
- `connect-src` は manifest `permissions.connect_allowlist` のみ許可する。
- 推奨ヘッダー:
  - `Content-Security-Policy`
  - `X-Content-Type-Options: nosniff`
  - `Referrer-Policy: strict-origin-when-cross-origin`
  - `Cross-Origin-Opener-Policy: same-origin`
  - `Cross-Origin-Resource-Policy: cross-origin`
- path traversal 防止:
  - `..` を含む path を reject する。
  - `/` は `entry_path` (default: `index.html`) にフォールバックする。

---

## 7. `postMessage` 契約

- Sandbox -> Theater 送信イベント:
  - `ato.play.ready`
  - `ato.play.resize`
  - `ato.play.permission.request`
  - `ato.play.error`
  - `ato.play.auth.request`
- Theater -> Sandbox 送信イベント:
  - `ato.play.auth.byok`
  - `ato.play.auth.tvm`
  - `ato.play.auth.clear`
- Theater は `event.origin === sandbox_origin` を必須検証する。
- Sandbox の `targetOrigin` は `play origin` を固定し、`*` を禁止する。

### 7.1 認証ポリシー

`GET /v1/capsules/:id/playground` は以下を返す。

```json
{
  "auth_policy": {
    "mode": "none | byok | tvm",
    "providers": ["openai"],
    "scopes": ["chat.completions"],
    "proxy_url": "https://proxy.ato.run/openai",
    "turnstile_site_key": "optional",
    "token_ttl_sec": 300
  }
}
```

- TVM mint (`POST /v1/tvm/mint`) は optional `surface` を受け入れる。
  - `surface = theater | embed`
  - 省略時は `theater`
- 発行 JWT claim に `surface` を含め、proxy 側でクォータ判定に利用する。

---

## 8. エラー契約

- `GET /v1/capsules/:id/playground`
  - `409 playground_not_available` (`none` / `pending`)
  - `423 playground_suspended`
  - `404 playground_not_ready | not_found`
- Edge Worker
  - `404` 未承認・停止・manifest 不在
  - `400` 不正 path

---

## 9. 運用ポリシー

- `PLAYGROUND_ADMIN_TOKEN` は publisher 認証と分離する。
- 管理UIは token をブラウザ配布せず、server-to-server proxy のみで使用する。
- 承認・停止は監査ログへ記録する。
- Phase 1 の観測は Workers Logs を一次系とする。
- ClickHouse 連携は Phase 2 で追加する。
