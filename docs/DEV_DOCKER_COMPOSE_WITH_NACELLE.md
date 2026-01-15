# 開発者向け: Docker Compose アプリを nacelle で起動する（Supervisor Mode）

このドキュメントは **既存の docker-compose.yml ベースの開発体験**を、nacelle の **Supervisor Mode（複数プロセス管理）**で再現するための手順です。

nacelle は Docker Compose の完全互換を目指していません。Compose の「複数コンテナ」をそのまま再現するのではなく、基本は **1つのホスト環境（または1つのサンドボックス）内で複数プロセスを管理**します。

- 実装根拠: `capsule.toml` の `[services]` と `depends_on` / `expose` / `readiness_probe`
- 非ゴール（現時点）: Compose の volumes / networks / replicas / Swarm/ECS 互換

---

## 0. 重要な前提（現状の制約）

- `capsule.toml` に `[services]` がある場合、**Supervisor Mode が発動**します。
- Supervisor Mode は **self-extracting bundle（`nacelle-bundle`）として実行したとき**に確実に動きます。
  - `nacelle internal exec`（開発用の単発起動）は現状 **単一プロセス**のみです。
- Supervisor Mode は、各サービスを `sh -c <entrypoint>` として起動します。
  - つまり `entrypoint` は **シェル文字列**です（クォート/リダイレクト等が使える）。

---

## 1. Compose から nacelle へ「考え方」を写経する

Docker Compose の概念を、nacelle の `[services]` にマッピングします。

| Docker Compose | nacelle（capsule.toml） | 備考 |
|---|---|---|
| `services.<name>` | `[services.<name>]` | 1サービス=1プロセス（複数OK） |
| `command` / `entrypoint` | `entrypoint = "..."` | `sh -c` で実行 |
| `depends_on` | `depends_on = ["..."]` | 起動順序 + readiness待ち（probe設定時） |
| `environment` | `env = { KEY = "VALUE" }` | `{{...}}` テンプレートでポート注入可 |
| `ports` | `expose = ["PORT"]` + `{{PORT}}` | nacelle は空きポートを確保して注入 |
| `healthcheck` | `readiness_probe = { ... }` | HTTP/TCP の簡易readiness |

### nacelle のテンプレート（ポート注入）

`expose = ["PORT", "ADMIN_PORT"]` のように **ポート名**を宣言すると、nacelle が空きポートを確保し、次を解決します。

- ローカル: `{{PORT}}` / `{{ADMIN_PORT}}`
- クロスサービス: `{{services.api.ports.PORT}}`

---

## 2. capsule.toml の最小例（Compose相当）

例として「API（Python）+ Web（Node）」の2サービス構成を想定します。

```toml
schema_version = "1.0"
name = "my-compose-like-app"
version = "0.1.0"
type = "app"

# 互換のために execution は残してOK（servicesがあると services 側が優先される）
[execution]
runtime = "source"
entrypoint = "noop"

# API service
[services.api]
entrypoint = "python server.py --port {{PORT}}"
expose = ["PORT"]
readiness_probe = { http_get = "/health", port = "PORT" }

# Web service
[services.web]
entrypoint = "node web.js --api http://127.0.0.1:{{services.api.ports.PORT}}"
depends_on = ["api"]
expose = ["WEB_PORT"]
readiness_probe = { tcp_connect = "127.0.0.1", port = "WEB_PORT" }
```

ポイント:
- `readiness_probe.port` は **数値**または **exposeで宣言したポート名**（例: `"PORT"`）を指定します。
- `http_get` は `"/health"` のようなパスでもOK（内部で `http://127.0.0.1:<port>/...` に展開）。

---

## 3. “DB/Redis などインフラ” は Compose 継続（おすすめ）

Compose で DB/Redis を起動し、アプリ（web/api）だけを nacelle に寄せるのが一番簡単です。

### 例: Compose（infraだけ）

```yaml
services:
  db:
    image: postgres:16
    ports:
      - "5432:5432"
    environment:
      POSTGRES_PASSWORD: password
  redis:
    image: redis:7
    ports:
      - "6379:6379"
```

### nacelle 側（アプリは localhost に向ける）

```toml
[services.api]
entrypoint = "python server.py --port {{PORT}}"
expose = ["PORT"]
env = { DATABASE_URL = "postgres://postgres:password@127.0.0.1:5432/postgres" }
```

---

## 4. 実行手順（self-extracting bundle を作って起動）

Supervisor Mode を使う最短手順です。

### 4.1 事前準備: runtime（実行バイナリ）と CLI（pack用）

`nacelle` workspace には「実行用ランタイム」と「pack用CLI（internal）」が共存しています。
現状、bundle に埋め込むランタイムは **Supervisor Mode 対応の方**を選ぶ必要があります。

```bash
cd nacelle

# 1) Supervisor Mode を含むランタイム（package: nacelle）をビルド
cargo build --release -p nacelle

# 2) 上書き衝突を避けるため、別名で退避（重要）
cp target/release/nacelle target/release/nacelle-runtime

# 3) pack の実行に使う CLI（package: nacelle-cli）
cargo build --release -p nacelle-cli
```

### 4.2 bundle を生成する（internal pack）

`nacelle internal pack` は JSON over stdio です。例では入力JSONをファイルに置きます。

```bash
cd /path/to/your/app

cat > nacelle-pack.json <<'JSON'
{
  "spec_version": "0.1.0",
  "workload": {
    "type": "source",
    "manifest": "./capsule.toml"
  },
  "output": {
    "format": "bundle",
    "path": "./nacelle-bundle"
  }
}
JSON

# bundle に埋め込むランタイムを明示（Supervisor Mode 対応バイナリ）
export NACELLE_BINARY="/absolute/path/to/nacelle/target/release/nacelle-runtime"

# pack 実行（stdoutはJSONレスポンス）
/path/to/nacelle/target/release/nacelle internal --input nacelle-pack.json pack
```

成功すると `./nacelle-bundle` ができます。

### 4.3 bundle を実行する

```bash
./nacelle-bundle
```

- `depends_on` と `readiness_probe` がある場合、依存サービスが ready になるまで待ってから次を起動します。
- どれか1サービスが終了すると fail-fast で全体を止めます（開発向け）。

---

## 5. よくあるハマりどころ（トラブルシュート）

### 5.1 `No capsule.toml found in bundle`

- `capsule.toml` がアプリのルートにあるか確認してください。
- `workload.manifest` のパス指定が正しいか確認してください。

### 5.2 `Service 'X' has no exposed port named 'PORT'`

- `entrypoint` / `env` / `readiness_probe` に `{{PORT}}` を使っている場合、`expose = ["PORT"]` を追加してください。

### 5.3 `Readiness probe timed out`

- `readiness_probe.http_get` のパスが正しいか、`port` が正しいか確認してください。
- 起動に時間がかかるサービスは、まず `readiness_probe` を外して起動順序だけで動くか確認すると切り分けが早いです。

### 5.4 bundle 内の実行ランタイム（Python/Node）が見つからない

Supervisor Mode はサービスを `sh -c` で起動するため、`python` や `node` が PATH に無いと失敗します。

- 開発環境: ホストに Python/Node を入れる（最短）
- bundle で同梱したい: `entrypoint` を `../runtime/...` で明示する

例（Pythonをbundle同梱している場合）:

```toml
[services.api]
entrypoint = "../runtime/python/bin/python3 server.py --port {{PORT}}"
expose = ["PORT"]
```

---

## 6. 現時点で未提供のこと（期待値調整）

- `docker-compose.yml` を直接読み込んで実行する importer（例: `--from-compose`）は未実装です。
- volumes/network driver/replicas など、Compose の完全再現はスコープ外です。
- Supervisor Mode は dev-first で、socket activation / sandbox の per-service 適用はまだ統合されていません。

---

## 参考

- `reports/ADR-003-nacelle-supervisor-mode.md`（Supervisor Mode の設計背景）
- `nacelle/src/engine/supervisor_mode.rs`（depends_on / expose / readiness_probe の実装）
- Docker Compose 仕様（depends_on / healthcheck 等の意味）
