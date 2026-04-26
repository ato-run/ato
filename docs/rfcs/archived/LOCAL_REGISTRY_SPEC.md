---
title: "Local Registry Spec (MVP)"
status: accepted
date: "2026-02-17"
author: "@egamikohsuke"
ssot:
  - "apps/ato-cli/"
related:
  - "STORE_SPEC.md"
---

# Local Registry Spec (MVP)

## 1. 目的

- 完全オフラインの `build -> publish(artifact) -> install -> run` を成立させる。
- 本番 Store API の最小読み取り面をローカルHTTPで再現する。
- 実装責務は「HTTP API をローカル FS にマッピングする」ことのみ。

## 2. 非目標

- 認証/認可
- 課金・ライセンス
- マルチテナント
- Cloud Store の write API 完全互換 (`POST /v1/capsules` など)

## 2.1 UI Reuse Boundary

- ローカルレジストリの埋め込み UI は `/Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-cli/apps/ato-store-local` に置く。
- ただし read-only の Dock 表示は store-web 由来の shared package を再利用する。
  - `/Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/packages/dock-domain`
  - `/Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/packages/dock-react`
- shared 範囲は以下に限定する。
  - catalog list / capsule card
  - detail summary
  - README panel
  - release table
  - type/category/trust の normalize
- local shell に残す責務は以下。
  - Run / Stop / Open / Delete
  - process logs / process drawer
  - runtime config / env / port / permission mode
  - rollback / yank
  - `store.metadata` 編集
- Cargo build 時の埋め込み UI 再ビルドは `apps/ato-cli/build.rs` が `apps/ato-store-local` に加えて `packages/dock-*` の変更も監視する。

## 3. CLI

- `ato registry serve [--port <u16>] [--data-dir <path>] [--host <ip>]`
  - `--host` は `127.0.0.1` のみ
  - 既定: `127.0.0.1:8787`
- `ato publish --artifact <path.capsule> --scoped-id <publisher/slug> --registry <http://127.0.0.1:port> [--json]`
  - `--scoped-id` 必須
  - `--registry` 必須
  - `http://127.0.0.1:<port>` 以外は拒否

## 4. 永続化モデル

- 既定保存先: `~/.capsule/local-registry`
- 構造:
  - `index.json`
  - `artifacts/<publisher>/<slug>/<version>/<file_name>.capsule`

### 4.1 `index.json` (v1)

```json
{
  "schema_version": "local-registry-v1",
  "capsules": [
    {
      "id": "local-koh0920-test-local",
      "publisher": "koh0920",
      "slug": "test-local",
      "name": "test-local",
      "description": "",
      "category": "tools",
      "type": "app",
      "price": 0,
      "currency": "usd",
      "latest_version": "1.0.0",
      "releases": [
        {
          "version": "1.0.0",
          "file_name": "test-local.capsule",
          "sha256": "sha256:...",
          "blake3": "blake3:...",
          "size_bytes": 12345,
          "signature_status": "verified",
          "created_at": "2026-02-24T00:00:00Z"
        }
      ],
      "downloads": 0,
      "created_at": "2026-02-24T00:00:00Z",
      "updated_at": "2026-02-24T00:00:00Z"
    }
  ]
}
```

## 5. HTTP API (MVP)

1. `GET /.well-known/capsule.json`
2. `GET /v1/capsules`
3. `GET /v1/capsules/by/:publisher/:slug`
4. `GET /v1/capsules/by/:publisher/:slug/distributions`
5. `GET /v1/capsules/by/:publisher/:slug/download`
6. `GET /v1/artifacts/:publisher/:slug/:version/:file_name`
7. `PUT /v1/local/capsules/:publisher/:slug/:version?file_name=<name>`

shared UI の read path は以下も利用してよい。

- `GET /v1/manifest/capsules`
- `GET /v1/manifest/capsules/by/:publisher/:slug`

これらは local shell / shared adapter 向けの正規化済み読み取り面であり、ローカル専用 write API (`/v1/local/*`) は引き続き shell 側に残す。

## 6. Upload 契約

### Request

- Method: `PUT`
- Headers:
  - `X-Ato-Sha256`
  - `X-Ato-Blake3`
- Body: `.capsule` binary (`application/octet-stream`)

### Response `201`

```json
{
  "scoped_id": "koh0920/test-local",
  "version": "1.0.0",
  "artifact_url": "http://127.0.0.1:8787/v1/artifacts/koh0920/test-local/1.0.0/test-local.capsule",
  "file_name": "test-local.capsule",
  "sha256": "sha256:...",
  "blake3": "blake3:...",
  "size_bytes": 12345
}
```

### Errors

- `400`: `scoped_id` / `manifest.name` / `manifest.version` 不整合
- `409`: 同一 `publisher+slug+version` が既存
- `422`: hash mismatch

## 7. 互換条件

- `install.rs` が要求する以下の読み取り契約を満たす:
  - `/v1/capsules/by/:publisher/:slug`
  - `/v1/capsules/by/:publisher/:slug/distributions`
  - `/v1/capsules/by/:publisher/:slug/download` (302 redirect)
- `search.rs` が要求する `GET /v1/capsules` 形状を満たす。
- `publisher` は固定:
  - `handle = <publisher>`
  - `authorDid = "did:key:local:<publisher>"`
  - `verified = true`
