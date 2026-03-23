# ato-cli

日本語 | [English](README.md)

`ato` は `capsule.toml` を解釈して、実行・配布・インストールを行うメタCLIです。  
Zero-Trust / fail-closed を前提に、通常実行時は静かに動作し、同意や違反時のみ明示的に出力します。

## 主要コマンド

```bash
ato run [path|publisher/slug|github.com/owner/repo] [--registry <url>]
ato open [path] [--watch]                 # 互換コマンド（非推奨; run を推奨）
ato ps
ato close --id <capsule-id> | --name <name> [--all] [--force]
ato logs --id <capsule-id> [--follow]
ato install <publisher/slug> [--registry <url>]
ato install --from-gh-repo <github.com/owner/repo>
ato build [dir] [--strict-v3] [--force-large-payload]
ato publish [--registry <url>] [--artifact <file.capsule>] [--scoped-id <publisher/slug>] [--allow-existing] [--prepare] [--build] [--deploy] [--legacy-full-publish] [--fix] [--no-tui] [--force-large-payload]
ato publish --dry-run
ato publish --ci
ato gen-ci
ato search [query]
ato source sync-status --source-id <id> --sync-run-id <id> [--registry <url>]
ato source rebuild --source-id <id> [--ref <branch|tag|sha>] [--wait] [--registry <url>]
ato config engine install --engine nacelle [--version <ver>]
ato setup --engine nacelle [--version <ver>] # 互換コマンド（非推奨）
ato registry serve --host 127.0.0.1 --port 18787 [--auth-token <token>]
```

## Native Delivery（実験的）

- 基本の product surface は引き続き `ato build` / `ato publish` / `ato install` です。
- 現在の Tauri darwin/arm64 PoC では、`capsule.toml` を正本として扱います。既定 target に `driver = "native"` と `.app` の `entrypoint` があれば native build を検出します。
- `ato.delivery.toml` は互換用 sidecar として引き続き受け付けます。存在する場合は `capsule.toml` の native target 契約と一致している必要があり、build 時に互換 metadata を artifact へ再生成します。
- native install の JSON では `local_derivation` と `projection` を返します。この世代の fetch/finalize/project/unproject/install metadata の stable な machine-readable version は `schema_version = "0.1"` です。
- `fetch` / `finalize` / `project` / `unproject` は advanced/debug surface の位置付けです。通常は統合済みの `build` / `publish` / `install` を使ってください。
- local finalize は fail-closed で、現時点では macOS darwin/arm64 + `codesign` のみ対応です。

### Native Delivery contract（現在の canonical 形）

project input の source of truth は `capsule.toml` です。現在の canonical contract は次です。

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

この `.app` entrypoint 形式では、現行 PoC の既定値は `ato` が内部導出します。

- `artifact.framework = "tauri"`
- `artifact.stage = "unsigned"`
- `artifact.target = "darwin/arm64"`
- `artifact.input = <targets.<default>.entrypoint>`
- `finalize.tool = "codesign"`
- `finalize.args = ["--deep", "--force", "--sign", "-", <artifact.input>]`

native target が command mode（`entrypoint = "sh"` と `cmd = [...]`）のときは、現時点では明示的な delivery metadata が必要です。その metadata は `capsule.toml` 内の `[artifact]` + `[finalize]` でも、互換 sidecar の `ato.delivery.toml` でも構いません。片方だけの inline metadata は fail-closed で拒否されます。

### 互換 sidecar と artifact metadata flow

- `ato.delivery.toml` はもはや canonical な project manifest ではありません。command-mode native build の互換入力、および `.app` entrypoint project 向けの互換 mirror metadata です。
- build は、source project が canonical `capsule.toml` しか持っていない場合でも、常に `ato.delivery.toml` を artifact payload に stage します。これにより source tree がなくても local finalize/install で artifact 自身が自己記述的になります。
- `ato install` / `ato finalize` / `ato project` は stage 済み artifact metadata と `local-derivation.json` を読みます。後段では source checkout 側の sidecar は不要です。
- 直近の方針としては、`ato.delivery.toml` は backward-compatible input として残しつつ、product surface 上では optional な compatibility metadata として扱います。

### Stable / experimental の machine-readable contract

現在の `schema_version = "0.1"` 世代では、repo 内で presence を test している machine-readable field を次の contract として扱います:

- `fetch.json`: `schema_version`, `scoped_id`, `version`, `registry`, `parent_digest`
- build JSON: `build_strategy = "native-delivery"`, `schema_version`, `target`, `derived_from`
- finalize JSON: `schema_version`, `derived_app_path`, `provenance_path`, `parent_digest`, `derived_digest`
- `local-derivation.json`: `schema_version`, `parent_digest`, `derived_digest`, `framework`, `target`, `finalize_tool`, `finalized_at`
- project JSON: `schema_version`, `projection_id`, `metadata_path`, `projected_path`, `derived_app_path`, `parent_digest`, `derived_digest`, `state`
- unproject JSON: `schema_version`, `projection_id`, `metadata_path`, `projected_path`, `removed_projected_path`, `removed_metadata`, `state_before`
- install JSON: `install_kind`, `launchable`, `local_derivation`, `projection`
  - `install_kind = "NativeRequiresLocalDerivation"` は install 自体は成功だが、起動対象が保存済み `.capsule` ではなく locally derived app bundle であることを意味します
  - `launchable.path` は caller が open/run に使うべき path です
  - `local_derivation.provenance_path`, `parent_digest`, `derived_digest` は fetch/finalize/project/install を結ぶ stable link です
  - `projection.metadata_path` は `ato unproject` や launcher state inspection に使う stable handle です

この保証は意図的に narrow です。field の追加はありえますが、上記の documented field の削除や rename は schema version を変える変更として扱う想定です。

引き続き experimental なもの:

- `~/.ato/fetches`, `~/.ato/apps`, `~/.ato/native-delivery/projections` 配下の正確な directory layout
- 上記 stable fields 以外で将来追加される key
- advanced/debug command (`fetch`, `finalize`, `project`, `unproject`) の UX 詳細。ただし現行の `schema_version = "0.1"` JSON envelope 自体は維持対象です
- macOS darwin/arm64 + `codesign` 以外の host/tool support

### Migration path

1. **現在**: `capsule.toml` が canonical。`ato.delivery.toml` は互換 input / output metadata として残す。
2. **次段**: command-mode native build は維持しつつ、docs / tooling は canonical `capsule.toml` を第一に案内し、manifest 情報だけで足りる場面では sidecar を optional にする。
3. **将来**: internal artifact metadata は sidecar 名から抽象化できるようにしつつ、automation 向けには `schema_version = "0.1"` の JSON / provenance contract を維持する。

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
./target/debug/ato open . --watch

# バックグラウンド管理
./target/debug/ato run . --background
./target/debug/ato ps
./target/debug/ato logs --id <capsule-id> --follow
./target/debug/ato close --id <capsule-id>
```

## 公開モデル（公式 / Dock / カスタム）

- 公式レジストリ（`https://api.ato.run`, `https://staging.api.ato.run`）:
  `ato publish` は CI-first（OIDC）で公開します。ローカルからの直接アップロードは行いません。
  既定フェーズは `deploy` のみ（handoff/diagnostics）です。ローカルで build 検証が必要な場合は `--build`（必要なら `--prepare --build`）を明示してください。
- Personal Dock（`ato login` 済みで `--registry` 未指定時の既定先）:
  `ato publish` はログイン済みユーザーの `https://store.ato.run/d/<handle>` を自動解決して直接アップロードします。
  `--artifact` 指定を推奨します（再パッキング回避）。`--scoped-id` 未指定時は `<handle>/<slug>` が自動採用されます。
- カスタム/私設レジストリ（上記以外の `--registry`）:
  `ato publish --registry ...` で直接アップロードします。`--artifact` 指定を推奨します（再パッキング回避）。
  `--artifact` はローカル `capsule.toml` がなくても単体で publish できます。
  `--allow-existing` は private/local の deploy フェーズ（`--deploy`）でのみ利用できます。

`ato publish` は固定順 `prepare -> build -> deploy` の3フェーズで実行されます。

- フェーズ指定なし（official）: `deploy` のみ実行
- フェーズ指定なし（private/local）: 3フェーズすべて実行
- `--prepare/--build/--deploy` のいずれか指定時: 指定フェーズのみ実行
- `--artifact` 指定時: build フェーズは常に skip
- `official + deploy` は handoff のみ（ローカル upload はしない）
- `--legacy-full-publish`（official専用）は旧既定（`prepare -> build -> deploy`）へ一時的に戻す互換フラグです。非推奨で、次回メジャーリリースで削除予定です。
- `--ci` / `--dry-run` とフェーズ指定は併用不可

公式レジストリ向け補助コマンド:

- `ato gen-ci` は OIDC publish 用の GitHub Actions ワークフローを生成します。
- `ato publish --fix` は公式 workflow の不足を一度だけ自動修正し、その後に診断を再実行します。
- `ato publish --no-tui` は対話 UI を出さず、CI 向けガイダンスをそのまま表示します。

## Dock-first フロー（Personal Dock）

Dock-first では既存コマンドだけで運用します（新サブコマンドなし）。

1. `ato login` を一度実行し、Store Web の `/publish` で Dock を作成/接続
2. ローカルで artifact 作成: `ato build .`
3. Personal Dock へ publish:
   `ato publish --artifact ./<name>.capsule`
4. 公開 Dock ページ `/d/<handle>` を共有
5. 公式 Store に出す段階になったら `ato publish --registry https://api.ato.run` または `ato publish --ci` を使う
6. 最終的な審査・提出は Dock Control Tower の `Submit to Official Marketplace` から進める

```bash
# 一度 login したら Personal Dock へそのまま publish（既定）
ato login
ato build .
ato publish --artifact ./<name>.capsule

# 事前ビルド + custom/private registry へ直接 publish
ato build .
ATO_TOKEN=pwd ato publish --registry http://127.0.0.1:18787 --artifact ./<name>.capsule

# フェーズ指定の実行例
ato publish --prepare
ato publish --build
ato publish --artifact ./<name>.capsule          # 既定ターゲット: My Dock
ATO_TOKEN=pwd ato publish --deploy --artifact ./<name>.capsule --registry http://127.0.0.1:18787
ato publish --registry https://api.ato.run           # 既定: deployのみ
ato publish --registry https://api.ato.run --build   # 明示的にローカルbuild + official handoff
ato publish --deploy --registry https://api.ato.run

# 一時互換フラグ（official専用・非推奨・次回メジャーで削除予定）
ato publish --registry https://api.ato.run --legacy-full-publish

# 同一 version/同一内容の再実行を成功扱いにする（idempotent / CI再試行の推奨）
ATO_TOKEN=pwd ato publish --registry http://127.0.0.1:18787 --artifact ./<name>.capsule --allow-existing
```

## Proto 再生成（メンテナンス時のみ）

通常ビルドでは `protoc` は不要です。  
`core/proto/tsnet/v1/tsnet.proto` を変更したときだけ、次を実行してください。

```bash
./core/scripts/gen_tsnet_proto.sh
```

## Source 同期操作

source 起点のレジストリ運用では、次のコマンドを使います。

```bash
# sync run の状態確認
ato source sync-status --source-id <source-id> --sync-run-id <sync-run-id> --registry <url>

# rebuild / re-sign を起動し、必要なら完了まで待つ
ato source rebuild --source-id <source-id> --ref <branch|tag|sha> --wait --registry <url>
```

補足:

- `sync-status` は読み取り専用で、`--json` にも対応します。
- `rebuild` は `--ref` を省略できます。その場合はレジストリ既定の ref を使います。
- `rebuild --wait` は rebuild 起動後、その sync run の状態を追跡します。

## ローカルレジストリ E2E

```bash
# ターミナル1: ローカルHTTPレジストリ起動
ato registry serve --host 127.0.0.1 --port 18787

# ターミナル2: build -> publish(artifact) -> install -> run
ato build .
ATO_TOKEN=pwd ato publish --artifact ./<name>.capsule --registry http://127.0.0.1:18787
ato install <publisher>/<slug> --registry http://127.0.0.1:18787
ato run <publisher>/<slug> --registry http://127.0.0.1:18787 --yes
```

補足:

- 書き込み（publish）は `ATO_TOKEN` が必要です（`registry serve --auth-token` 設定時）。
- 読み取り（search/install/download）は無認証のまま利用できます。
- ローカル検証では `18787` を使うと、worker 等が `8787` を使うアプリとのポート衝突を避けられます。
- `publish --artifact` はローカル用途向けの推奨経路です。
- `--scoped-id` で artifact upload 時の publisher/slug を明示指定できます。
- `--allow-existing` は単なる競合無視ではなく、artifact hash / manifest 整合性チェック付きの冪等操作です。
- エンタープライズCIの再試行経路では、`--allow-existing` を付与して再実行を安全に決定論化することを推奨します。
- version 競合は `E202` で返り、次アクション（version更新 / `--allow-existing` / ローカルレジストリ初期化）を表示します。

ローカルレジストリ Web UI:

- 詳細画面は `/v1/local/.../runtime-config` に target ごとの runtime config を保存します。
- UI から target 別の `env` / `port` override を保存できます。
- Tier2 target では実行権限モード (`sandbox` / `dangerous`) も保存でき、次回実行時に再利用されます。

## 別デバイス公開（VPN / Tailscale 想定）

```bash
# サーバー側: 非loopback公開時は --auth-token 必須
ato registry serve --host 0.0.0.0 --port 18787 --auth-token pwd

# クライアント側: install/run は token 不要（読み取りAPI）
ato install <publisher>/<slug> --registry http://100.x.y.z:18787
ato run <publisher>/<slug> --registry http://100.x.y.z:18787

# パブリッシュ時のみ token 必須
ATO_TOKEN=pwd ato publish --registry http://100.x.y.z:18787 --artifact ./<name>.capsule
```

## 実行前の環境変数チェック

`ato run` は起動前に必須環境変数を検証します。未設定または空文字なら fail-closed で停止します。

- `targets.<label>.required_env = ["KEY1", "KEY2"]`（推奨）
- 既存互換: `targets.<label>.env.ATO_ORCH_REQUIRED_ENVS = "KEY1,KEY2"`

## 動的アプリのカプセル化手順（Web + Services Supervisor）

複数サービス（例: dashboard + API + worker）を1つのカプセルで動かす場合は、トップレベルの `[services]` を持つ `web/deno` ターゲット1つに統一します。`ato run` は DAG 順にサービスを起動し、readiness probe を待ち、ログに service prefix を付け、どれか1つが終了したら fail-fast で全体停止します。

1. パッキング前に成果物を事前ビルドする（例: `next build`、worker build、lockfile）。
2. `[pack].include` で実行成果物だけを同梱する（生の `node_modules`、`.venv`、キャッシュは同梱しない）。
3. `ato build` で一度だけ作成し、`publish --artifact` で再パッキングを避ける。

最小構成の `capsule.toml` 例:

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

# 3) 成果物をpublish（private/local registry）
ATO_TOKEN=pwd ato publish --registry http://127.0.0.1:18787 --artifact ./my-dynamic-app.capsule

# 4) install + run
ato install <publisher>/<slug> --registry http://127.0.0.1:18787
ato run <publisher>/<slug> --registry http://127.0.0.1:18787
```

補足:

- Next.js standalone は `ato build` 前に `.next/static`（必要なら `public` も）を standalone 出力へコピーしてください。
- `required_env` 未設定時、`ato run` は起動前に停止します。
- `services.main` は services モードで必須で、`PORT=<targets.<label>.port>` を受け取ります。
- `targets.<label>.entrypoint = "ato-entry.ts"` は非推奨で、現在は拒否されます。
- service command が `node` / `python` / `uv` で始まる場合は、対応する version を `runtime_tools` に固定してください。

## Build の strict モード

`ato build --strict-v3` は、`source_digest` / CAS(v3 path) が使えない場合のフォールバックを禁止します。
manifest 側への緩いフォールバックではなく、その場で build 診断を失敗させたい場合に使います。

## ランタイム隔離ポリシー（Tier）

- `web/static`: Tier1（`driver = "static"` + `targets.<label>.port` 必須。`capsule.lock.json` 不要）
- `web/deno`: Tier1（`capsule.lock.json` + `deno.lock` または `package-lock.json`）
- `web/node`: Tier1（Deno compat 実行。`capsule.lock.json` + `package-lock.json` 必須）
- `web/python`: Tier2（`uv.lock` 必須、`--sandbox` 推奨）
- `source/deno`: Tier1（`capsule.lock.json` + `deno.lock` または `package-lock.json`）
- `source/node`: Tier1（Deno compat 実行。`capsule.lock.json` + `package-lock.json` 必須）
- `source/python`: Tier2（`uv.lock` 必須、`--sandbox` 推奨）
- `source/native`: Tier2（`--sandbox` 推奨）

補足:

- Node は Tier1 として `--unsafe` 不要です。
- Tier2（`source/native|python`, `web/python`）は `nacelle` エンジンが必須です。
  未登録時は fail-closed で停止するため、事前に `ato engine register` か `--nacelle` / `NACELLE_PATH` で設定してください。
- Legacy 互換で `--unsafe` / `--unsafe-bypass-sandbox` は残っていますが、利用は非推奨です。
- Node/Python で非対応・逸脱が発生した場合は自動フォールバックせず fail-closed で停止します。
- `runtime=web` は `driver` が必須です（`static|node|deno|python`）。
- `runtime=web` では `public` は廃止されました。
- `runtime=web` 実行時、CLI は URL を表示します（ブラウザ自動起動はしません）。

## Run 入力の現行仕様

`ato run` は次の入力を受け付けます。

- ローカルディレクトリ
- ローカルの `capsule.toml`
- ローカルの `.capsule`
- `publisher/slug`
- `github.com/owner/repo`

補足:

- `--skill` と `--from-skill` は削除されました。
- `open` エイリアスも削除され、実行コマンドは `ato run` に統一されています。

## UX方針（Silent Runner）

- 正常時は最小出力（ツールの標準出力中心）
- 同意が必要なときのみプロンプト表示
- 非対話環境では `-y/--yes` で同意を自動承認できます
- ポリシー違反や未充足は `ATO_ERR_*` JSONL を `stderr` に出力

## セキュリティと実行ポリシー（Zero-Trust / Fail-closed）

- 必須環境変数検証: `targets.<label>.required_env`（または `ATO_ORCH_REQUIRED_ENVS`）が未設定/空文字なら起動前に停止
- 危険フラグ制御: `--dangerously-skip-permissions` は `CAPSULE_ALLOW_UNSAFE=1` がない限り拒否
- ローカルレジストリ書き込み認証: `registry serve --auth-token` 利用時、publish は `ATO_TOKEN` 必須
- エンジン自動取得: チェックサム取得/検証に失敗した場合は fail-closed で停止

## 環境変数リファレンス（主要）

- `CAPSULE_WATCH_DEBOUNCE_MS`: `open --watch` のデバウンス間隔（ms, default: `300`）
- `CAPSULE_ALLOW_UNSAFE`: `--dangerously-skip-permissions` の明示許可（`1` のみ有効）
- `ATO_TOKEN`: ローカル/私設レジストリへの publish 認証トークン
- `ATO_STORE_API_URL`: `ato search` / install 系で使う API ベースURL（default: `https://api.ato.run`）
- `ATO_STORE_SITE_URL`: ストアWebのベースURL（default: `https://store.ato.run`）
- `ATO_TOKEN`: ヘッドレス/CI 用のセッション認証トークン

## 検索・認証

```bash
ato search ai
ato login
ato whoami
```

既定API:

- `ATO_STORE_API_URL` (default: `https://api.ato.run`)
- `ATO_STORE_SITE_URL` (default: `https://store.ato.run`)
- `ATO_TOKEN`

## 開発用テスト

```bash
cargo test -p capsule-core execution_plan:: --lib
cargo test -p ato-cli --test local_registry_e2e -- --nocapture
```

## License

Apache License 2.0 (SPDX: Apache-2.0). See [LICENSE](LICENSE).
