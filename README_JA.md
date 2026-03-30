# ato-cli

日本語 | [English](README.md)

`ato` は capsule を実行・配布・インストールするためのコマンドラインツールです。

`ato` を実行すると、プロジェクト内の `ato.lock.json` を設定の基準として読み込みます。まだ `ato.lock.json` がない場合でも、旧形式の設定ファイルが使えます。準備ができたら `ato init` で移行してください。

`ato` は普段は静かに動きます。権限の確認が必要なときや、ポリシー違反が起きたときだけ出力します。

## 主要コマンド

```bash
ato run [path|publisher/slug|github.com/owner/repo] [--registry <url>]
ato ps
ato close --id <capsule-id> | --name <name> [--all] [--force]
ato logs --id <capsule-id> [--follow]
ato install <publisher/slug> [--registry <url>]
ato install --from-gh-repo <github.com/owner/repo>
ato build [dir] [--strict-v3] [--force-large-payload]
ato publish [--registry <url>] [--artifact <file.capsule>] [--scoped-id <publisher/slug>] [--allow-existing] [--prepare] [--build] [--deploy] [--legacy-full-publish] [--fix] [--no-tui] [--force-large-payload]
ato publish --dry-run
ato publish --ci
ato init [path] [--yes]
ato gen-ci
ato inspect lock [path] [--json]
ato inspect preview [path] [--json]
ato inspect diagnostics [path] [--json]
ato inspect remediation [path] [--json]
ato search [query]
ato source sync-status --source-id <id> --sync-run-id <id> [--registry <url>]
ato source rebuild --source-id <id> [--ref <branch|tag|sha>] [--wait] [--registry <url>]
ato config engine install --engine nacelle [--version <ver>]
ato setup --engine nacelle [--version <ver>] # 互換コマンド（非推奨）
ato registry serve --host 127.0.0.1 --port 18787 [--auth-token <token>]
```

`ato inspect` には、lock の問題をデバッグするためのコマンドも含まれています。

- `ato inspect lock [path] [--json]` — 各 lock フィールドの解決結果・出所・未解決の項目などを表示します
- `ato inspect preview [path] [--json]` — `ato init` や `ato run` がどのファイルを書くか、実際には変更せずに確認できます
- `ato inspect diagnostics [path] [--json]` — lock 設定の問題点と、修正のためのコマンドを案内します
- `ato inspect remediation [path] [--json]` — 修正方法を提案します。可能な場合はソースの場所も表示します

## lock-native 入力モデル

`ato.lock.json` が設定の基準（唯一の正本）です。このファイルがあれば、他の設定は無視されるか、参考情報として扱われます。

各設定形式の関係をまとめると:

- **`ato.lock.json`**: メインの設定ファイルです。存在する場合は常に優先されます。
- **旧形式・bootstrap 形式**: `ato.lock.json` に移行前のプロジェクトでも引き続き使えます。`ato init` で移行できます。
- **`capsule.lock.json`**: 互換性のための旧形式です。`ato.lock.json` と併用できますが、単独では使えません。

`ato inspect lock`・`ato run`・`ato build` はすべて、この優先順位に従って動作します。

## Native Delivery（実験的）

> **注意:** 基本コマンド（`ato build`・`ato publish`・`ato install`）は安定しています。Native Delivery はその上に追加された実験的な機能です。

Native Delivery を使うと、ネイティブのデスクトップアプリを配布できます。現在は macOS darwin/arm64 の Tauri アプリのみ対応しています。

**使い方:**

project manifest に native ターゲットを追加します。`driver = "native"` を設定して、`entrypoint` に `.app` バンドルのパスを指定するだけです。これだけで `ato` が native アプリとして認識します。

```toml
[targets.desktop]
driver = "native"
entrypoint = "MyApp.app"
```

**`ato` が自動で設定するもの:**

`.app` 形式の entrypoint の場合、次のデフォルト値を `ato` が内部で導出します。自分で書く必要はありません。

- `artifact.framework = "tauri"`
- `artifact.stage = "unsigned"`
- `artifact.target = "darwin/arm64"`
- `artifact.input = <targets.<default>.entrypoint>`
- `finalize.tool = "codesign"`
- `finalize.args = ["--deep", "--force", "--sign", "-", <artifact.input>]`

**現在の制限:**

- macOS darwin/arm64 + `codesign` のみ対応しています。
- `fetch`・`finalize`・`project`・`unproject` はデバッグ・上級者向けのコマンドです。通常は `build` / `publish` / `install` を使ってください。
- local finalize はエラーがあれば即停止します（fail-closed）。
- Projection は macOS では `~/Applications` にシンボリックリンクを作成します。Linux では `.desktop` ランチャーと `~/.local/bin` シンボリックリンクを作成します。

**command-driven ターゲットの場合:**

`entrypoint = "sh"` と `cmd = [...]` を使う command-driven モードでは、`[artifact]` と `[finalize]` に明示的な設定が必要です。片方だけでは動きません。source 側の `ato.delivery.toml` は常に拒否されます。すべて project manifest に書いてください。

### Native Delivery の設定形式（現在の canonical 形）

native delivery 向けの最小構成の project manifest:

```toml
schema_version = "0.2"
name = "my-app"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "native"
entrypoint = "MyApp.app"
```

### artifact メタデータの流れ

`ato.delivery.toml` は `ato` が内部で管理するファイルです。直接書く必要はありません。内部ではこのように動いています。

1. `ato build` を実行すると、アプリをパッケージ化しながら `ato.delivery.toml` を artifact の中に埋め込みます。
2. この埋め込みファイルのおかげで、後から `ato install` や `ato finalize` を実行するとき、元のソースコードがなくても動きます。
3. `ato install`・`ato finalize`・`ato project` はすべて、この埋め込みファイルを参照します。

**方針:** native delivery の設定は project manifest に書いてください。`ato.delivery.toml` を直接書く必要はありません。

### Stable / experimental な machine-readable contract

`schema_version = "0.1"` では、次の field を stable な contract として扱います。これらの削除や rename はスキーマバージョンを変える変更として扱います。

- `fetch.json`: `schema_version`, `scoped_id`, `version`, `registry`, `parent_digest`
- build JSON: `build_strategy = "native-delivery"`, `schema_version`, `target`, `derived_from`
- finalize JSON: `schema_version`, `derived_app_path`, `provenance_path`, `parent_digest`, `derived_digest`
- `local-derivation.json`: `schema_version`, `parent_digest`, `derived_digest`, `framework`, `target`, `finalize_tool`, `finalized_at`
- project JSON: `schema_version`, `projection_id`, `metadata_path`, `projected_path`, `derived_app_path`, `parent_digest`, `derived_digest`, `state`
- unproject JSON: `schema_version`, `projection_id`, `metadata_path`, `projected_path`, `removed_projected_path`, `removed_metadata`, `state_before`
- install JSON: `install_kind`, `launchable`, `local_derivation`, `projection`
  - `install_kind = "NativeRequiresLocalDerivation"` — install 自体は成功。ただし起動には locally derived app bundle を使います（`.capsule` ではありません）
  - `launchable.path` — アプリを起動するときに使うパスです
  - `local_derivation.provenance_path`, `parent_digest`, `derived_digest` — fetch・finalize・project・install の各ステップをつなぐリンクです
  - `projection.metadata_path` — `ato unproject` や launcher 状態確認に使います

まだ experimental なもの:

- `~/.ato/fetches`・`~/.ato/apps`・`~/.ato/native-delivery/projections` 配下のディレクトリ構造
- 上記の stable field 以外に将来追加される field
- `fetch`・`finalize`・`project`・`unproject` の UX 詳細
- macOS darwin/arm64 以外のプラットフォームサポート

### Migration path

1. **現在**: native delivery の設定は project manifest にのみ書きます。
2. **現在**: `ato` は引き続き `ato.delivery.toml` を artifact に埋め込みます（内部の finalize/install/project フロー用）。
3. **将来**: 内部の artifact メタデータ形式を変えても、`schema_version = "0.1"` の JSON contract は維持します。

## クイックスタート（ローカル）

```bash
# build
cargo build -p ato-cli

# nacelle エンジンを未導入の場合（推奨）
./target/debug/ato config engine install --engine nacelle

# 互換: setup サブコマンド
./target/debug/ato setup --engine nacelle

# 実行
./target/debug/ato run .

# 開発時ホットリロード
./target/debug/ato run . --watch

# バックグラウンド管理
./target/debug/ato run . --background
./target/debug/ato ps
./target/debug/ato logs --id <capsule-id> --follow
./target/debug/ato close --id <capsule-id>
```

## 公開モデル（公式 / Dock / カスタム）

`ato publish` は最大6つのステージを順番に実行します:

**`Prepare → Build → Verify → Install → Dry-run → Publish`**

どこに公開するかによって、実行されるステージと動作が変わります。

---

### 公式 Store に公開する

**レジストリ:** `https://api.ato.run` または `https://staging.api.ato.run`

公式 Store は OIDC 認証を使います。ローカルから直接アップロードすることはできません。CI 経由で公開します。

デフォルト動作: **Publish** ステージのみ実行します。これは handoff です — `ato` が diagnostics をレジストリに送り、実際のアップロードは CI パイプラインが行います。

---

### Personal Dock に公開する

**レジストリ:** `ato login` 済みで `--registry` を指定しない場合のデフォルト

`ato login` を済ませていれば、`ato publish` は自動的に Dock にアップロードします。`--registry` フラグは不要です。

デフォルト動作: Prepare から Publish まですべてのステージを実行します。

現在の managed direct upload 制限:

- Personal Dock の direct upload は現在 managed Store の direct-upload path を使います。
- artifact が現在の conservative preflight limit である 95 MB を超える場合は、upload 前に reject されます。
- `--force-large-payload` と `--paid-large-payload` はこの path では使えません。
- 現時点でより大きい direct upload が必要なら、custom/private registry を使ってください。

P1 の移行用 experimental path:

- `ATO_PUBLISH_UPLOAD_STRATEGY=presigned` を設定すると、互換 registry に対して新しい presigned upload strategy を明示 opt-in できます。
- Managed Store の capability discovery は現在 `presigned` を default upload strategy として advertise しており、ato-cli は explicit override が無い限りそちらを選びます。
- presigned strategy には、認証済み publisher session と、publisher onboarding で作成された local publisher signing key が必要です。
- Managed Store の presigned publish は、`--allow-existing` parity と、registered だが未検証の publisher account を使う flow にも対応しました。
- Managed Store の direct upload は現在、explicit な rollback/debug path としてだけ残しています。custom/private registry では direct upload を通常経路として引き続き使えます。

ポイント:

- `--artifact <file>` を使えば再ビルドをスキップできます。すでにビルド済みのファイルをそのままアップロードできます。
- `--scoped-id` は未指定時に `<ハンドル>/<スラッグ>` が自動設定されます。
- Dock のページは `/d/<ハンドル>` にあります。これは UI ページであり、レジストリのエンドポイントではありません。

---

### カスタム・私設レジストリに公開する

**レジストリ:** 上記以外の `--registry <url>`

デフォルト動作: Prepare から Publish まですべてのステージを実行して、直接アップロードします。

ポイント:

- `--artifact <file>` はここでも使えます。ローカルの project manifest がなくても公開できます。
- `--allow-existing` は最終の Publish ステージでのみ使えます。
- `--force-large-payload` と `--paid-large-payload` は、managed Store direct-upload policy の対象外である custom/private direct registry では引き続き使えます。

---

### 実行するステージを絞る

`--prepare`・`--build`・`--deploy` は「ここで止まる」を指定するフラグです。個別のステージを選ぶトグルではありません。

| フラグ              | 停止タイミング | 補足                                                                                 |
| ------------------- | -------------- | ------------------------------------------------------------------------------------ |
| `--prepare`         | Prepare 後     |                                                                                      |
| `--build`           | Verify 後      | source 入力: Build → Verify を実行。`--artifact` 指定: Verify のみ実行               |
| `--deploy`          | Publish 後     | 公式: handoff のみ。private/local: 自動解決、または `--artifact` で Verify → Publish |
| `--artifact <file>` | _(start 変更)_ | Prepare・Build をスキップして Verify から開始                                        |

注意点:

- `--ci` / `--dry-run` とステージフラグは同時に使えません。
- `--artifact --prepare` は無効です（start が stop より後になるため）。
- `--legacy-full-publish`（official 専用）は旧デフォルト動作への一時的な互換フラグです。**非推奨** — 次回メジャーリリースで削除予定です。

公式レジストリ向け補助コマンド:

- `ato gen-ci` — OIDC 公開用の GitHub Actions ワークフローを生成します。
- `ato publish --fix` — workflow の問題を一度だけ自動修正し、再度 diagnostics を実行します。
- `ato publish --no-tui` — 対話 UI を出さずに CI 向けの出力を直接表示します。

### Publish payload limitation (`E212`)

`E212` は、managed Store publish path が現在の payload 構成を受け付けなかった、または禁止したことを意味します。

典型的な原因:

- artifact が managed direct upload の current conservative preflight limit を超えている
- `--force-large-payload` または `--paid-large-payload` を managed direct-upload path で使った
- remote managed upload path が `413 Payload Too Large` を返した

推奨アクション:

- artifact size を下げる
- 今すぐ direct upload が必要なら custom/private registry に publish する
- official Store には CI-first の publish flow を使う
- より大きい payload が必要なら managed presigned upload 対応を待つ

### Migration Notes

- `ato publish --build` は Build 直後ではなく、Verify の後に停止するようになりました。
- `ato run --skill` と `ato run --from-skill` は削除されました。

## Dock-first フロー（Personal Dock）

Dock への公開の典型的な流れ:

1. **ログイン:** `ato login`
   その後、Store Web の `/publish` ページで Dock を作成・接続します。
2. **ビルド:** `ato build .`
3. **Dock に公開:** `ato publish --artifact ./<name>.capsule`
4. **ページを共有:** 公開 Dock ページは `/d/<ハンドル>` にあります。
5. **公式 Store に申請する場合:** `ato publish --registry https://api.ato.run` または `ato publish --ci` を使います。
6. **審査・提出:** Dock Control Tower の `Submit to Official Marketplace` から進めます。

```bash
# 一度 login したら Personal Dock へそのまま publish（推奨）
ato login
ato build .
ato publish --artifact ./<name>.capsule

# 事前ビルド + custom/private registry へ直接 publish
ato build .
ATO_TOKEN=pwd ato publish --registry http://127.0.0.1:18787 --artifact ./<name>.capsule

# ステージ指定の実行例
ato publish --prepare
ato publish --build                               # Prepare -> Build -> Verify
ato publish --artifact ./<name>.capsule --build  # Verify のみ
ato publish --artifact ./<name>.capsule          # デフォルトターゲット: My Dock
ATO_TOKEN=pwd ato publish --deploy --artifact ./<name>.capsule --registry http://127.0.0.1:18787
ato publish --registry https://api.ato.run           # デフォルト: Publish のみ
ato publish --registry https://api.ato.run --build   # ローカル build + verify、その後停止
ato publish --deploy --registry https://api.ato.run

# 一時互換フラグ（official 専用・非推奨・次回メジャーで削除予定）
ato publish --registry https://api.ato.run --legacy-full-publish

# 同一 version・同一内容の再実行を安全に行う（CI 再試行の推奨方法）
ATO_TOKEN=pwd ato publish --registry http://127.0.0.1:18787 --artifact ./<name>.capsule --allow-existing
```

## Proto 再生成（メンテナンス時のみ）

通常のビルドでは `protoc` は不要です。`core/proto/tsnet/v1/tsnet.proto` を変更したときだけ実行してください:

```bash
./core/scripts/gen_tsnet_proto.sh
```

## Source 同期操作

source 起点のレジストリ運用では、次のコマンドを使います:

```bash
# sync run の状態確認
ato source sync-status --source-id <source-id> --sync-run-id <sync-run-id> --registry <url>

# rebuild を起動し、必要なら完了まで待つ
ato source rebuild --source-id <source-id> --ref <branch|tag|sha> --wait --registry <url>
```

- `sync-status` は読み取り専用です。`--json` で機械可読な出力が得られます。
- `rebuild` は `--ref` を省略できます。省略するとレジストリのデフォルトブランチが使われます。
- `rebuild --wait` は rebuild を起動した後、完了するまで状態を追跡します。

## ローカルレジストリ E2E

```bash
# ターミナル1: ローカル HTTP レジストリを起動
ato registry serve --host 127.0.0.1 --port 18787

# ターミナル2: build → publish → install → run
ato build .
ATO_TOKEN=pwd ato publish --artifact ./<name>.capsule --registry http://127.0.0.1:18787
ato install <publisher>/<slug> --registry http://127.0.0.1:18787
ato run <publisher>/<slug> --registry http://127.0.0.1:18787 --yes
```

いくつかのポイント:

- **書き込み操作**（publish など）は、`registry serve --auth-token` で起動した場合に `ATO_TOKEN` が必要です。
- **読み取り操作**（search・install・download）は認証なしで使えます。
- ポート `18787` を使うと、`8787` を使うアプリ（worker の HTTP ポートなど）との衝突を避けられます。
- ローカル・私設レジストリでは `publish --artifact` が推奨の経路です。
- `--scoped-id` で upload 時の publisher/slug を上書き指定できます。
- `--allow-existing` は「競合を無視する」フラグではありません。artifact hash と manifest の整合性を確認したうえで、再アップロードを受け付ける冪等な操作です。
- CI の再試行経路では `--allow-existing` を付けると、安全かつ再現性のある再実行ができます。
- version 競合は `E202` として報告され、次のアクション（version 更新・`--allow-existing`・ローカルレジストリ初期化）が案内されます。

**ローカルレジストリ Web UI:**

詳細画面では `/v1/local/.../runtime-config` に target ごとの設定を保存できます。UI から `env` と `port` の override を設定できます。Tier2 target では実行権限モード（`sandbox` / `dangerous`）も保存でき、次回実行時に引き継がれます。

## 別デバイス公開（VPN / Tailscale）

```bash
# サーバー側: 非 loopback で公開する場合は --auth-token が必須
ato registry serve --host 0.0.0.0 --port 18787 --auth-token pwd

# クライアント側: install・run は token 不要（読み取り専用 API）
ato install <publisher>/<slug> --registry http://100.x.y.z:18787
ato run <publisher>/<slug> --registry http://100.x.y.z:18787

# publish のみ token が必要
ATO_TOKEN=pwd ato publish --registry http://100.x.y.z:18787 --artifact ./<name>.capsule
```

## 実行前の環境変数チェック

`ato run` は起動前に必須環境変数をすべて確認します。未設定または空文字の変数があれば、そこで停止します。

manifest での設定方法:

```toml
# 推奨
targets.<label>.required_env = ["KEY1", "KEY2"]

# 旧形式（互換）
targets.<label>.env.ATO_ORCH_REQUIRED_ENVS = "KEY1,KEY2"
```

## Inspect Requirements JSON

```bash
ato inspect requirements <path|publisher/slug> --json
```

capsule の実行に必要なものを機械可読な形式で返します。シークレット・環境変数・ネットワークアクセス・サービス・同意プロンプトなどが含まれます。

`ato inspect` ファミリーは、lock のデバッグにも使えます:

- `ato inspect lock [path] [--json]` — 各 lock フィールドの解決結果・出所・未解決の項目などを表示します
- `ato inspect preview [path] [--json]` — `ato init` や `ato run` がどのファイルを書くか、実際には変更せずに確認できます
- `ato inspect diagnostics [path] [--json]` — lock 設定の問題点と修正のためのコマンドを案内します
- `ato inspect remediation [path] [--json]` — 修正方法を提案します。可能な場合はソースの場所も示します

補足:

- 要件は常に project manifest から読み込まれます（lock ファイルからではありません）。
- ローカルパスと `publisher/slug` のリモート参照は、同じ JSON 形式で返ります。
- state 関連の要件は `requirements.state` に含まれます（`storage` ではありません）。
- 成功時は `stdout` に JSON を出力します。`--json` 付きで失敗した場合は `stderr` に構造化 JSON を出力してゼロ以外で終了します。

成功時の形式:

```json
{
  "schemaVersion": "1",
  "target": {
    "input": "./examples/foo",
    "kind": "local",
    "resolved": {
      "path": "/abs/path/to/examples/foo"
    }
  },
  "requirements": {
    "secrets": [],
    "state": [],
    "env": [],
    "network": [],
    "services": [],
    "consent": []
  }
}
```

失敗時の形式:

```json
{
  "error": {
    "code": "CAPSULE_TOML_NOT_FOUND",
    "message": "project manifest was not found",
    "details": {
      "input": "./examples/foo"
    }
  }
}
```

## Build の strict モード

`ato build` はデフォルトで、content-addressed なソースダイジェストが使えない場合に緩いパスへフォールバックします。

`--strict-v3` を使うとそのフォールバックを無効にします。`source_digest` や CAS v3 パスが使えない場合は即エラーになります。問題を早期に検出したいときに便利です。

```bash
ato build --strict-v3
```

## 動的アプリのカプセル化（Web + Services Supervisor）

dashboard・API・worker などの複数のサービスを1つのカプセルにまとめるには、`[services]` を使います。

カプセルを起動すると `ato run` は次の動作をします:

- 依存関係の順番（DAG 順）でサービスを起動する
- 各サービスの readiness probe が通るまで待つ
- すべてのログにサービス名のプレフィックスを付ける
- いずれかのサービスが終了したら、全体を即停止する

**パッケージ化の前に:**

1. アプリを事前ビルドしておきます（`next build`・worker のビルド・lockfile 生成など）。ソースコードはカプセルに含めません。
2. `[pack].include` には実行時に必要なファイルだけを含めます。`node_modules`・`.venv`・キャッシュは含めないでください。
3. `ato build` でビルドしたら、`--artifact` で再ビルドせずに publish します。

最小構成の project manifest 例:

```toml
schema_version = "0.2"
name = "my-dynamic-app"
version = "0.1.0"
default_target = "default"

[pack]
include = [
  "capsule.toml",
  "capsule.lock.json",
  "apps/dashboard/.next/standalone/**",
  "apps/dashboard/.next/static/**",
  "apps/control-plane/src/**",
  "apps/control-plane/pyproject.toml",
  "apps/control-plane/uv.lock",
  "apps/worker/src/**",
  "apps/worker/wrangler.dev.jsonc"
]
exclude = [
  ".deno/**",
  "node_modules/**",
  "**/__pycache__/**",
  "apps/dashboard/.next/cache/**"
]

[targets.default]
runtime = "web"
driver = "deno"
runtime_version = "1.46.3"
runtime_tools = { node = "20.11.0", python = "3.11.10", uv = "0.4.30" }
port = 4173
required_env = ["CLOUDFLARE_API_TOKEN", "CLOUDFLARE_ACCOUNT_ID"]

[services.main]
entrypoint = "node apps/dashboard/.next/standalone/server.js"
depends_on = ["api"]
readiness_probe = { http_get = "/health", port = "PORT" }

[services.api]
entrypoint = "python apps/control-plane/src/main.py"
env = { API_PORT = "8000" }
readiness_probe = { http_get = "/health", port = "API_PORT" }
```

推奨フロー:

```bash
# 1) 事前ビルド
npm run capsule:prepare

# 2) カプセル化
ato build .

# 3) 成果物を publish（private/local registry）
ATO_TOKEN=pwd ato publish --registry http://127.0.0.1:18787 --artifact ./my-dynamic-app.capsule

# 4) install + run
ato install <publisher>/<slug> --registry http://127.0.0.1:18787
ato run <publisher>/<slug> --registry http://127.0.0.1:18787
```

注意点:

- Next.js standalone を使う場合は、`ato build` 前に `.next/static`（必要なら `public` も）を standalone の出力先にコピーしてください。
- `required_env` のキーが未設定だと、`ato run` は起動前に停止します。
- `services.main` は必須です。`PORT=<targets.<label>.port>` を受け取ります。
- `targets.<label>.entrypoint = "ato-entry.ts"` は非推奨です。現在は拒否されます。
- service コマンドが `node`・`python`・`uv` で始まる場合は、`runtime_tools` に対応するバージョンを指定してください。

## ランタイム隔離ポリシー（Tier）

runtime ごとに必要な条件と隔離レベルが異なります:

| ランタイム      | Tier  | 必要なもの                                                      |
| --------------- | ----- | --------------------------------------------------------------- |
| `web/static`    | Tier1 | `driver = "static"` + port の設定（`capsule.lock.json` 不要）   |
| `web/deno`      | Tier1 | `capsule.lock.json` + `deno.lock` または `package-lock.json`    |
| `web/node`      | Tier1 | `capsule.lock.json` + `package-lock.json`（Deno compat で実行） |
| `web/python`    | Tier2 | `uv.lock`・`--sandbox` 推奨                                     |
| `source/deno`   | Tier1 | `capsule.lock.json` + `deno.lock` または `package-lock.json`    |
| `source/node`   | Tier1 | `capsule.lock.json` + `package-lock.json`（Deno compat で実行） |
| `source/python` | Tier2 | `uv.lock`・`--sandbox` 推奨                                     |
| `source/native` | Tier2 | `--sandbox` 推奨                                                |

**Tier1** は特別なフラグなしで動きます。Node は Tier1 なので `--unsafe` は不要です。

**Tier2**（`source/native`・`source/python`・`web/python`）は `nacelle` エンジンが必要です。未設定の場合は起動前に停止します。次のいずれかで設定してください:

```bash
ato engine register     # エンジンのパスを登録する
ato run --nacelle ...   # 実行時にパスを渡す
# または NACELLE_PATH 環境変数を設定する
```

その他:

- `--unsafe`・`--unsafe-bypass-sandbox` は旧互換フラグとして残っていますが、使用は推奨しません。
- 非対応・逸脱が起きてもサイレントなフォールバックはしません。エラーで停止します。
- `runtime=web` には `driver` の指定が必須です（`static`・`node`・`deno`・`python`）。
- `runtime=web` では `public` は廃止されました。
- `runtime=web` では URL を表示しますが、ブラウザは自動起動しません。

## Run 入力の現行仕様

`ato run` は次の入力を受け付けます:

- ローカルディレクトリ
- ローカルの project manifest
- ローカルの `.capsule` ファイル
- `publisher/slug`
- `github.com/owner/repo`

なお、`--skill` と `--from-skill` は削除されました。

## UX 方針（Silent Runner）

`ato` は邪魔にならない設計になっています:

- **正常終了時**: 出力は最小限。ツールの stdout をそのまま表示します。
- **同意プロンプト**: 本当に必要なときだけ表示します。
- **非対話環境**: `-y` / `--yes` で自動承認できます。
- **エラー**: ポリシー違反や未充足の要件は `ATO_ERR_*` JSONL として `stderr` に出力します。

## セキュリティと実行ポリシー（Zero-Trust / Fail-closed）

`ato` はデフォルトで厳格に動作します:

- **必須環境変数の検証**: `targets.<label>.required_env`（または `ATO_ORCH_REQUIRED_ENVS`）に列挙した変数が未設定または空文字の場合、`ato run` は起動前に停止します。
- **危険フラグ**: `CAPSULE_ALLOW_UNSAFE=1` がない限り、`--dangerously-skip-permissions` は拒否されます。
- **レジストリの書き込み認証**: `--auth-token` 付きで起動したレジストリへの publish には `ATO_TOKEN` が必要です。
- **エンジン取得**: チェックサムの検証に失敗した場合、実行を停止します。

## 環境変数リファレンス（主要）

| 変数                        | 説明                                                                         | デフォルト              |
| --------------------------- | ---------------------------------------------------------------------------- | ----------------------- |
| `CAPSULE_WATCH_DEBOUNCE_MS` | `run --watch` のデバウンス間隔（ミリ秒）                                     | `300`                   |
| `CAPSULE_ALLOW_UNSAFE`      | `1` に設定すると `--dangerously-skip-permissions` が使えます                 | —                       |
| `ATO_TOKEN`                 | ローカル・私設レジストリへの publish 認証トークン。CI セッションにも使います | —                       |
| `ATO_STORE_API_URL`         | `ato search` / install で使う API の URL                                     | `https://api.ato.run`   |
| `ATO_STORE_SITE_URL`        | ストア Web の URL                                                            | `https://store.ato.run` |

## 検索・認証

```bash
ato search ai
ato login
ato whoami
```

デフォルトの接続先:

- `ATO_STORE_API_URL`（default: `https://api.ato.run`）
- `ATO_STORE_SITE_URL`（default: `https://store.ato.run`）
- `ATO_TOKEN`

認証情報を探す順番:

1. `ATO_TOKEN` 環境変数
2. OS キーリング
3. `${XDG_CONFIG_HOME:-~/.config}/ato/credentials.toml`
4. 旧形式 `~/.ato/credentials.json`（読み取り専用のフォールバック）

## 開発用テスト

```bash
cargo test -p capsule-core execution_plan:: --lib
cargo test -p ato-cli --test local_registry_e2e -- --nocapture
```

## License

Apache License 2.0 (SPDX: Apache-2.0). See [LICENSE](LICENSE).
