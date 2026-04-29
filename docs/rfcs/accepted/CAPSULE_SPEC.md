---
title: "Capsule Manifest Specification for ato-cli (Current)"
status: accepted
date: "2026-03-14"
author: "@egamikohsuke"
ssot:
  - "apps/ato-cli/core/src/types/manifest.rs"
---

# Capsule Manifest Specification for ato-cli (Current)

この文書は、現在の `ato-cli` が実際に読み取っている `capsule.toml` 契約をまとめたものである。将来案や Store/Theater 側の理想仕様ではなく、`ato open` / `ato build` / `ato validate` / `ato inspect requirements` / `ato publish` で使われる現行実装を優先する。

## 1. 位置づけ

- プロジェクト作成時の canonical な authoring 形式は `schema_version = "0.3"`
- `"0.3"` 以外の値は validation error として拒否する
- 旧 `[execution]` セクションは parse error として拒否する
- 実行対象はすべて `[targets.<label>]` 配下で定義する
- `ato-cli` の `open/build/validate` は named target 前提で動く。旧ドラフトの `targets.source` / `targets.wasm` / `targets.oci` だけに依存した manifest は、現行 CLI の主契約ではない

---

## 2. 必須トップレベル項目

| 項目 | 現在の扱い | 備考 |
| --- | --- | --- |
| `schema_version` | Required | `"0.3"` のみ受理する |
| `name` | Required | kebab-case、長さ 3..64 |
| `version` | Required | semver |
| `type` | Required | `app` / `tool` / `inference` |
| `default_target` | Required | `[targets]` に実在する label を指す必要がある |
| `[targets.<label>]` | Required | 少なくとも 1 つ必要 |

最小例:

```toml
schema_version = "0.3"
name = "hello-capsule"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "native"
run = "./hello"
```

Node / Deno / Python を source target として使う場合は、`runtime_version` まで固定するのが現行契約である。

```toml
schema_version = "0.3"
name = "hello-python"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "python"
runtime_version = "3.11.10"
run = "main.py"
```

---

## 3. `[targets.<label>]` 契約

現在の `ato-cli` が読む主な target field は次のとおり。

| 項目 | 必須性 | 説明 |
| --- | --- | --- |
| `runtime` | Required | `source` / `web` / `wasm` / `oci` |
| `driver` | `runtime=web` では Required | 許可値は `static` / `deno` / `node` / `python` / `wasmtime` / `native` |
| `language` | Optional | `driver` 推論に使う |
| `runtime_version` | 条件付き Required | `source` で有効 driver が `deno` / `node` / `python` の場合に必要 |
| `runtime_tools` | Optional | 追加 tool version 固定。`web/deno` services mode では実質必須になることがある |
| `run` | 多くの runtime で Required | authoring 時のフィールド名。内部では `run_command` (source) または `entrypoint` (web/static) に正規化される |
| `image` | Optional | `runtime=oci` で推奨。`run` の代替としても扱われる |
| `env` | Optional | target 実行時の環境変数 |
| `required_env` | Optional | 起動前に fail-closed で存在確認する必須環境変数名 |
| `port` | `runtime=web` では Required | 1..65535 |
| `working_dir` | Optional | 実行時 working directory |

`public` は struct 上は残っているが、`runtime=web` では validation error になる。現行 CLI 仕様では廃止済み。

---

## 4. Runtime ごとのルール

### 4.1 `runtime = "source"`

- `run` は必須
- `driver` は省略可能
- `driver` 省略時は次の順で推論する
	- `run` コマンドの先頭が `deno` / `node` / `python`
	- `language`
	- どれにも当てはまらなければ `native`
- 許可 driver は `deno` / `node` / `python` / `native`
- `driver = "deno" | "node" | "python"` になる場合、`runtime_version` は必須

### 4.2 `runtime = "web"`

- `driver` は必須
- 許可 driver は `static` / `deno` / `node` / `python`
- `driver = "browser_static"` または `"browser-static"` は拒否する。現行値は `static`
- `port` は必須で、1..65535 の整数でなければならない
- `public` は廃止済みで、指定すると validation error
- 実行時、CLI は URL を表示するだけでブラウザ自動起動はしない

#### `runtime = "web", driver = "static"`

- `run` は必須
- `run` はプロジェクト root 配下の既存ディレクトリでなければならない
- `ato build` は web packer を使う

#### `runtime = "web", driver = "node" | "deno" | "python"`

- 通常モードでは `run` は必須
- `run` は shell command ではなく script file path でなければならない
- `run` は project root または `source/` 配下に実在する必要がある

#### Compound selector authoring (`runtime = "web/node"` 等)

- authoring では `runtime = "web/node"`, `"web/deno"`, `"web/python"`, `"web/static"` を受理できる
- normalizer はこれを `runtime = "web"` + `driver = "<suffix>"` として解釈する
- 実行計画の target summary では dynamic web driver (`node` / `deno` / `python`) が `runtime = "source"` + `driver = "<suffix>"` として表示されることがある
- この場合でも `render_strategy = "web"` と `port` が web 表示契約を保持するため、Desktop / web pane は web capsule として扱う
- `web/static` は静的配布契約なので `runtime = "web"` + `driver = "static"` を維持する

### 4.3 `runtime = "wasm"`

- 現行 `open/build/validate` 契約では `run` を要求する
- 実行計画上の driver は `wasmtime` として扱われる
- `services` orchestration の target としては使えない

### 4.4 `runtime = "oci"`

- `run` または `image` のいずれかが必要
- `image` 指定が推奨
- `services` orchestration では利用可能だが、OCI service は non-OCI service に依存できない

---

## 5. Services の 2 モード

現在の `ato-cli` では、トップレベル `[services]` は 2 通りの意味で使われる。

### 5.1 Orchestration mode

いずれかの service が `target = "..."` を持つと orchestration mode として扱う。

```toml
schema_version = "0.3"
name = "multi-service"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "web"
driver = "node"
run = "dist/server.js"
port = 3000

[targets.worker]
runtime = "source"
driver = "python"
runtime_version = "3.11.10"
run = "worker.py"

[services.main]
target = "app"
depends_on = ["jobs"]

[services.jobs]
target = "worker"
```

ルール:

- `[services]` は空であってはならない
- `services.main` は必須
- `target` と `run` は同時に指定できない
- `services.main` は `target` と `run` の両方を省略した場合、`default_target` を継承できる
- 各 `target` は `[targets.<label>]` に実在しなければならない
- `runtime=wasm` target は orchestration mode では使えない
- `depends_on` は既存 service のみ参照可能
- 依存 cycle は拒否する
- `service.network.aliases` と `allow_from` に空文字は許されない
- `allow_from` は既存 service 名のみ参照可能
- OCI service は non-OCI service に依存できない
- `readiness_probe` を書く場合、`port` は空であってはならず、`http_get` か `tcp_connect` のどちらかが必要
- `state_bindings` は top-level `state` で宣言済みの state 名のみ参照できる

### 5.2 Web/Deno services mode

選択 target が `runtime = "web"` かつ `driver = "deno"` で、トップレベル `[services]` が存在すると web/deno services mode として扱う。

```toml
schema_version = "0.3"
name = "dynamic-web-app"
version = "0.1.0"
type = "app"
default_target = "default"

[targets.default]
runtime = "web"
driver = "deno"
runtime_version = "1.46.3"
runtime_tools = { node = "20.11.0", python = "3.11.10", uv = "0.4.30" }
port = 4173
required_env = ["CLOUDFLARE_API_TOKEN"]

[services.main]
entrypoint = "node apps/web/server.js"
depends_on = ["api"]
readiness_probe = { http_get = "/health", port = "PORT" }

[services.api]
entrypoint = "python apps/api/main.py"
readiness_probe = { http_get = "/health", port = "API_PORT" }
```

ルール:

- top-level `[services]` は必須
- `services.main` は必須
- 各 `services.<name>.entrypoint` は必須
- `targets.<label>.entrypoint = "ato-entry.ts"` は拒否する
- `services.<name>.expose` は未サポートで、空配列以外は拒否する
- `depends_on` は既存 service のみ参照可能
- 依存 cycle は拒否する
- `readiness_probe` の制約は orchestration mode と同じ
- service command の先頭が `node` / `python` / `uv` の場合、対応する `targets.<label>.runtime_tools.<tool>` が必要
- このモードでは target 側の `run` は省略可能

---

## 6. `required_env` と環境変数契約

`ato run` / `ato open` は起動前に必須環境変数を検証する。値が未設定または空文字なら fail-closed で停止する。

推奨形式:

```toml
[targets.cli]
runtime = "source"
driver = "python"
runtime_version = "3.11.10"
run = "main.py"
required_env = ["OPENAI_API_KEY", "DATABASE_URL"]
```

既存互換:

```toml
[targets.cli.env]
ATO_ORCH_REQUIRED_ENVS = "OPENAI_API_KEY,DATABASE_URL"
```

`ato inspect requirements` は `required_env`、`isolation.allow_env`、`state`、`network`、`services` を requirement として列挙する。

---

## 7. その他の top-level table

現在の `ato-cli` が認識する代表的な top-level section は次のとおり。

| セクション | 用途 | 備考 |
| --- | --- | --- |
| `[metadata]` | UI / publish 用メタデータ | `display_name`, `description`, `author`, `icon`, `tags`。publish 系では `metadata.repository` も参照する |
| `[requirements]` | 実行要件 | `platform`, `vram_min`, `vram_recommended`, `disk`, `dependencies` |
| `[network]` | outbound allowlist | `egress_allow`, `egress_id_allow` |
| `[pack]` | パッキング include / exclude | 空文字 pattern は拒否する |
| `[isolation]` | host env passthrough allowlist | `allow_env` |
| `[state]` | host-managed filesystem state | service `state_bindings` の参照元 |
| `[storage]` | OCI 向け volume | volume があるのに OCI target がない場合は validation error |
| `[build]` | packaging / publish policy | `gpu`, lifecycle, inputs, outputs, policy など |
| `[routing]` | ルーティング hint | 現在も deserialize される |
| `[distribution]` | pack / publish 時の生成 metadata | 通常は手書きしない |
| `[store]` | Store 向け補助情報 | 現在の `ato-cli` は `store.playground = true` を publish hint として参照する |

`[playground.api]` は現在の `ato-cli` では読まれていない。CLI 実装基準の仕様としては対象外である。

---

## 8. Target 解決とコマンド挙動

### 8.1 Target 解決

- `ato open` / `ato build` / `ato validate` は、CLI 引数で target が明示されればそれを使う
- 明示がなければ `default_target` を使う
- 選択 label が `[targets]` に存在しなければエラー

### 8.2 Validation

- `ato validate` は選択 target に対して build-time validator を実行する
- `capsule.lock.json` が存在する場合は整合性も検証する
- IPC 関連 manifest 設定も追加で診断する

### 8.3 Web target の実行

- `runtime=web` target は `targets.<label>.port` を持つ必要がある
- CLI は `http://127.0.0.1:<port>/` を表示する
- ブラウザ自動起動は行わない

---

## 9. Native Delivery（実験的）

現在の `ato-cli` には experimental な native-delivery 分岐があり、`capsule.toml` から build strategy を切り替える。

最小契約:

```toml
schema_version = "0.3"
name = "my-app"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "native"
run = "MyApp.app"
```

補足:

- 現在の PoC は主に macOS darwin/arm64 + codesign 想定
- command-mode native build では `ato.delivery.toml` または inline delivery metadata が必要になる場合がある
- これは通常の source/web/wasm/oci 契約とは別の experimental surface である

---

## 10. 現行実装ベースの互換メモ

- authoring では `schema_version = "0.3"` を使う。他の値は validation error として拒否される
- `[execution]` は完全に廃止済み
- `[targets.<label>]` では `entrypoint` / `cmd` の代わりに `run` を使う
- `runtime=web` の `public` は廃止済み
- `driver = "browser_static"` は廃止済み
- `targets.<label>.entrypoint = "ato-entry.ts"` は v0.3 では `entrypoint` / `cmd` 自体が非対応のため拒否される

この文書は `ato-cli` の現状を記述するため、Store Playground のターゲット選択や Theater API 認証のような別コンポーネント固有仕様は含めない。
