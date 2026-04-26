# 📄 Lifecycle & Task Management Specification

**Document ID:** `LIFECYCLE_SPEC`
**Status:** Accepted v1.2
**Target:** nacelle v0.2+
**Last Updated:** 2026-02-06

## 1. 概要 (Overview)

本仕様は、Capsule アプリケーションの構築、実行、終了のライフサイクルを管理するための明示的な定義モデルを提供する。

### 1.1 解決する問題

従来の実装では、`capsule.toml` の `entrypoint` フィールドからランタイムやツールチェーンを**推測（Implicit）** していた。これにより以下の問題が発生していた:

| 問題               | 具体例                                                                 |
| ------------------ | ---------------------------------------------------------------------- |
| 言語推測の誤り     | `entrypoint = "uv"` → `"python"` と判定 → `-B` フラグ注入 → 起動失敗   |
| ツール別の分岐爆発 | `auto_install_deps` が `npm`/`pip`/`uv` ごとにハードコード             |
| 暗黙の依存解決     | `requirements.txt` や `package.json` の存在で自動推測 → 予測不能な動作 |

### 1.2 設計方針

- **明示的記述（Explicit）**: 「どうビルドし、どう動かすか」を開発者が `capsule.toml` に明示的に書く
- **Smart Build, Dumb Runtime**: DAG の定義（Smart）は設定ファイルの責務。nacelle は定義されたグラフを愚直に実行する（Dumb）
- **言語推測の廃止**: nacelle はコマンドの引数を改変しない。`-B` 等のフラグ注入は行わない
- **`[language]` セクションは JIT provisioning のヒントのみ**: ツールチェーンの事前ダウンロードには使うが、`run` コマンドの引数操作には使わない

## 2. コアコンセプト (Core Concepts)

システムは以下の3つの要素で構成される。

### 2.1 Tasks (タスク) — ワンショット実行

- 完了するもの。作用するもの。関数。ジョブ。
- 例: `build`, `test`, `migrate`, `backup`
- `depends_on` により依存関係（DAG）を持ち、実行順序が制御される。
- 正常終了（exit 0）で「完了」とみなされる。

### 2.2 Services (サービス) — 常駐プロセス

- 常駐するもの。状態を持つもの。サーバー。デーモン。
- 例: `httpd`, `llama-server`, `sidecar`
- `readiness_probe` により起動完了を判定する。
- プロセス終了は「異常」として扱われる（fail-fast）。

### 2.3 Lifecycle Hooks (ライフサイクルフック)

- Ato ランタイムが認識するシステムイベント（起動、終了）。
- タスクまたはサービスをフックにバインドすることで、アプリの振る舞いを決定する。

> **設計決定:** `[tasks]` と `[services]` を意味的に分離する。
> Docker Compose は `services:` にワンショットジョブを混在させるが、これは「ベストプラクティスに見えるバッドノウハウ」である。
> Ato は `[tasks]`（完了するもの）と `[services]`（常駐するもの）を明確に区別する。
>
> **内部実装:** パース時に `[tasks.*]` は `ServiceSpec { oneshot: true }` に変換される。
> `supervisor_mode.rs` の既存 DAG ソルバー・プロセス管理をそのまま再利用する。

## 3. スキーマ定義 (`capsule.toml`)

### 3.1 二層構造 — 「単純なものは単純に、複雑なことも可能に」

#### Level 1: ショートカット (`[lifecycle]` のみ、90%のケース)

`[tasks]` / `[services]` を定義せず、`[lifecycle]` に直接コマンドを書ける。
内部的に匿名タスク/サービスとして展開される（Syntactic Sugar）。

```toml
schema_version = "1.1"
name = "simple-app"
version = "1.0.0"
type = "app"

[lifecycle]
setup = "npm install && npm run build"
run = "npm start"
port = 3000
```

#### Level 2: 明示的定義 (`[tasks]` + `[services]` + `[lifecycle]`)

IPC、サイドカー、複雑な依存関係が必要な場合に使用する。

```toml
schema_version = "1.1"
name = "complex-app"
version = "1.0.0"
type = "app"

[tasks.install]
cmd = "npm ci"

[tasks.migrate]
cmd = "npx prisma migrate deploy"
depends_on = ["install"]

[services.app]
cmd = "npm start"
depends_on = ["migrate"]
expose = ["APP_PORT"]
readiness_probe = { http_get = "/health", port = "APP_PORT" }

[lifecycle]
run = "app"
stop = "snapshot"
stop_timeout = "30s"
```

### 3.2 Task Definition (`[tasks.*]`)

```toml
[tasks.<task_name>]
# 実行コマンド (必須)
cmd = "npm run build"

# タスクの説明 (CLIのhelp等で表示)
description = "Build the application"

# 依存タスク/サービスのリスト (この前に完了/readyである必要がある)
depends_on = ["install"]

# 環境変数 (このタスク固有)
env = { "NODE_ENV" = "production" }

# 作業ディレクトリ (デフォルトはカプセルルート)
cwd = "./backend"
```

タスクは常に **ワンショット（oneshot）** である。正常終了（exit 0）で完了、非ゼロ終了でエラー（後続は中止）。

### 3.3 Service Definition (`[services.*]`)

```toml
[services.<service_name>]
# 実行コマンド (必須)
cmd = "npm start"

# 依存タスク/サービスのリスト
depends_on = ["migrate"]

# ポート公開 (動的割り当て、環境変数として注入)
expose = ["APP_PORT"]

# 環境変数
env = { "NODE_ENV" = "production" }

# 作業ディレクトリ
cwd = "./backend"

# Readiness Probe (起動完了判定)
readiness_probe = { http_get = "/health", port = "APP_PORT" }
# または: readiness_probe = { tcp_connect = "APP_PORT", port = "APP_PORT" }
```

サービスは常に **デーモン（long-running）** である。プロセス終了は異常として扱われ、fail-fast で全体を停止する。

### 3.4 Lifecycle Binding (`[lifecycle]`)

```toml
[lifecycle]
# --- Level 1: 直接コマンド記述（ショートカット） ---
setup = "npm install"            # 起動前のワンショットタスク
run = "npm start"                # メインプロセス（デーモン）

# --- Level 2: タスク/サービス名の参照 ---
# run = "app"                    # [services.app] を参照
# stop = "snapshot"              # [tasks.snapshot] を参照

# --- ポート・ヘルスチェック ---
port = 3000                      # nacelle がこのポートを poll して ready 判定
health_check = "/api/config"     # HTTP 200 で ready

# --- 終了制御 ---
stop_signal = "SIGTERM"          # default: SIGTERM
stop_timeout = "30s"             # default: 10s

# --- 並列実行 (§7 参照) ---
parallel = false                 # default: false (逐次実行)
```

### 3.5 Language Hint (`[language]`) — JIT provisioning 専用

```toml
[language]
name = "node"     # ツールチェーンの事前ダウンロードのみに使用
version = "20"    # nacelle はこれを見てランタイムを確保するが
                  # run コマンドの引数を改変しない
```

> **重要:** nacelle は `[language]` に基づくフラグ注入（`-B` 等）を**一切行わない**。
> 開発者が `[tasks]` / `[services]` / `[lifecycle]` の `cmd` に必要な引数を全て明示する。

### 3.6 Build Lifecycle (`[build.lifecycle]`) — CI/配布専用

`[lifecycle]`（実行時）とは別に、配布物生成のための build フローを `build` セクションに定義する。

```toml
[build.lifecycle]
prepare = "npm ci"
build = "npm run build"
package = "ato pack"
verify = "ato verify --strict"
publish = "ato publish --ci"

[build.inputs]
lockfiles = ["package-lock.json"]
toolchain = "node:20"
artifacts = ["dist/**"]
allow_network = false
reproducibility = "best-effort"

[build.outputs]
capsule = "dist/*.capsule"
sha256 = true
blake3 = true
attestation = true
signature = true

[build.policy]
require_attestation = true
require_did_signature = true
```

- `build.lifecycle`: reusable CI action が実行する build DAG（Store/Nacelle は直接実行しない）
- `build.inputs`: provenance と監査のための宣言入力
- `build.outputs`: Store 側検証で期待する成果物
- `build.policy`: 公開ゲート判定（attestation / DID 署名）

## 4. 内部変換ルール (Desugaring)

ato-cli / ato-desktop のパース層が、ショートカット表記を内部表現に展開する。

| capsule.toml 記述                             | → nacelle 内部 `ServiceSpec`                                                        |
| --------------------------------------------- | ----------------------------------------------------------------------------------- |
| `[tasks.X]`                                   | `ServiceSpec { oneshot: true, ... }`                                                |
| `[services.X]`                                | `ServiceSpec { oneshot: false, ... }`                                               |
| `[lifecycle] setup = "cmd"`                   | `ServiceSpec { name: "_setup", oneshot: true, cmd: "cmd" }`                         |
| `[lifecycle] run = "cmd"`                     | `ServiceSpec { name: "_main", oneshot: false, cmd: "cmd", depends_on: ["_setup"] }` |
| `[lifecycle] run = "app"` (services 定義あり) | `app` サービスを main として指定                                                    |
| `[lifecycle] stop = "snapshot"`               | shutdown hook として登録                                                            |

nacelle の `supervisor_mode.rs` は統一された `ServiceSpec` を受け取り、DAG に従って実行する。元が `[tasks]` か `[services]` かは関知しない。

## 5. ランタイム動作仕様 (Runtime Behavior)

### 5.1 依存関係解決 (Dependency Resolution)

ランタイムはタスク/サービス実行前に DAG（有向非巡回グラフ）を構築し、レイヤー構造に分解する。

- **レイヤー分解:** DAG をトポロジカルソートし、`Vec<Vec<TaskID>>`（実行レイヤー）として生成する。
- **循環参照:** 循環（A → B → A）が検出された場合、起動前にエラーとして停止する。
- **相互参照:** `[tasks]` と `[services]` は相互に `depends_on` できる。

### 5.2 起動フロー (`ato open`)

1. **Graph Build:** `lifecycle.run` で指定されたサービスをルートとする依存グラフを構築。
2. **Layer Execution:** レイヤーごとにタスク/サービスを実行。
   - **oneshot（タスク）:** 正常終了を待ってから次のレイヤーに進む。失敗時は全体を中止。
   - **daemon（サービス）:** spawn 後、readiness probe を待ってから次のレイヤーに進む。
3. **Main Process:** 全依存が完了/ready 後、メインサービスが起動完了。
4. **Monitoring:** メインサービスの PID を監視し、予期せぬ終了を検知する。

### 5.3 終了フロー (`ato close` / User Action)

1. **Stop Hook:** `lifecycle.stop` が定義されている場合、そのタスクを実行。メインプロセスが生きた状態でデータを安全に退避できる。
2. **Signal Transmission:** `stop` タスク完了後、メインプロセスに `stop_signal`（SIGTERM）を送信。
3. **Grace Period:** `stop_timeout` の間、プロセスの正常終了を待機。
4. **Force Kill:** タイムアウトした場合、SIGKILL で強制終了。
5. **Cleanup:** 全サービスのプロセスツリーを逆順で停止。

## 6. CLI コマンド体系

| コマンド             | 説明                                        | 対象         |
| -------------------- | ------------------------------------------- | ------------ |
| `ato open`       | `lifecycle.run` のフルフロー（setup → run） | lifecycle    |
| `ato close`      | 終了フロー（stop → signal → cleanup）       | lifecycle    |
| `ato run [name]` | サービスを起動（常駐）                      | `[services]` |
| `ato do <task>`  | タスクを単体実行（完了で終了）              | `[tasks]`    |

`ato do migrate` のように、アプリを起動せずに特定のタスクだけを実行できる。

## 7. 並列実行 (Concurrency)

### 7.1 デフォルト挙動

デフォルトでは、依存関係解決後もタスクは **逐次（Sequential）** に実行される。これはログの可読性とローカルリソースの安全性を優先するためである。

- **ログの可読性:** 逐次実行では「上から順にログが流れる」ため、デバッグが容易
- **リソース競合の回避:** ローカル PC では巨大なビルドタスクの並列実行で I/O 飽和・フリーズのリスクがある
- **安全性:** 同一ファイルへのアクセス競合（Race Condition）を暗黙に回避

### 7.2 Opt-in 並列化

`capsule.toml` にて `parallel = true` が指定された場合、ランタイムは依存関係のないタスク同士を並列に実行する（Layered Execution）。

```toml
[lifecycle]
parallel = true
```

### 7.3 実行戦略

DAG ソルバーは実行計画を `Vec<Vec<TaskID>>`（レイヤー構造）として生成する。

```
例: [tasks.install] → [tasks.build, tasks.migrate] → [services.app]

Layer 0: [install]           ← 依存なし、単体実行
Layer 1: [build, migrate]    ← 両方 install にのみ依存 → parallel=true なら並列可
Layer 2: [app]               ← build と migrate 両方に依存
```

| `parallel`            | 挙動                                                           |
| --------------------- | -------------------------------------------------------------- |
| `false`（デフォルト） | レイヤーを平坦化し、前から順に1つずつ `await`                  |
| `true`                | 同一レイヤー内のタスクを `tokio::spawn` + `JoinSet` で並列実行 |

> **実装指示:** `resolve_dependencies` は `Vec<String>`（フラットリスト）ではなく `Vec<Vec<String>>`（レイヤー構造）を返すように変更すること。`parallel = false` でも内部的にはレイヤー構造を保持し、実行時に平坦化する。

## 8. 記述例 (Examples)

### 8.1 Level 1: シンプルなアプリ（ショートカット）

```toml
schema_version = "1.1"
name = "hello-app"
version = "1.0.0"
type = "app"

[lifecycle]
setup = "npm install"
run = "npm start"
port = 3000
```

### 8.2 Level 1: Python (uv)

```toml
schema_version = "1.1"
name = "python-streamlit"
version = "0.1.0"
type = "app"

[lifecycle]
setup = "uv sync"
run = "uv run streamlit run app.py --server.headless true"
port = 8501

[language]
name = "python"
version = "3.12"
```

### 8.3 Level 2: DB マイグレーション付きアプリ

```toml
schema_version = "1.1"
name = "chat-app"
version = "1.0.0"
type = "app"

[tasks.install]
cmd = "npm ci"
description = "Install dependencies"

[tasks.build]
cmd = "npm run build"
depends_on = ["install"]

[tasks.migrate]
cmd = "npx prisma migrate deploy"
depends_on = ["install"]

[tasks.snapshot]
cmd = "node scripts/save_ui_state.js"
env = { "OUTPUT_PATH" = "./data/state.json" }

[services.app]
cmd = "npm start"
depends_on = ["build", "migrate"]
expose = ["APP_PORT"]
readiness_probe = { http_get = "/health", port = "APP_PORT" }

[lifecycle]
run = "app"
stop = "snapshot"
stop_timeout = "15s"
```

### 8.4 Level 2: LLM サイドカー付きアプリ

```toml
schema_version = "1.1"
name = "ai-chat"
version = "1.0.0"
type = "app"

[services.llm]
cmd = "llama-server --model ./model.gguf --port {{LLM_PORT}}"
expose = ["LLM_PORT"]
readiness_probe = { http_get = "/health", port = "LLM_PORT" }

[services.app]
cmd = "npm start"
depends_on = ["llm"]
expose = ["APP_PORT"]
env = { "LLM_URL" = "http://localhost:{{services.llm.ports.LLM_PORT}}" }

[lifecycle]
run = "app"

[language]
name = "node"
version = "20"
```

## 9. 後方互換性 (Migration)

### 9.1 `schema_version = "1.0"` のサポート

`schema_version = "1.0"` の capsule.toml（`[lifecycle]` / `[tasks]` / `[services]` なし）は、従来の `[execution]` + `[build]` フィールドから自動変換される:

| 旧フィールド                       | → 新しい内部表現         |
| ---------------------------------- | ------------------------ |
| `[build] command`                  | `lifecycle.setup`        |
| `[execution] entrypoint + command` | `lifecycle.run`          |
| `[execution] port`                 | `lifecycle.port`         |
| `[execution] health_check`         | `lifecycle.health_check` |

### 9.2 `auto_install_deps` の段階的廃止

| フェーズ        | 挙動                                                                                      |
| --------------- | ----------------------------------------------------------------------------------------- |
| Phase 1（現行） | `lifecycle.setup` が定義されていれば使用、未定義なら `auto_install_deps` にフォールバック |
| Phase 2         | `auto_install_deps` 使用時に deprecation 警告を表示                                       |
| Phase 3         | `auto_install_deps` を完全削除                                                            |

## 10. 実装マッピング (Implementation Notes)

| 仕様の概念            | 既存実装の対応箇所                                                |
| --------------------- | ----------------------------------------------------------------- |
| DAG 解決              | `supervisor_mode.rs::resolve_dependencies`                        |
| ポート動的割り当て    | `supervisor_mode.rs::allocate_ports`                              |
| テンプレート変数注入  | `supervisor_mode.rs::inject_all_services`                         |
| Readiness Probe       | `supervisor_mode.rs::wait_for_readiness`                          |
| Fail-fast Supervision | `supervisor_mode.rs::run_supervisor_mode`                         |
| Graceful Shutdown     | `supervisor_mode.rs` (SIGTERM → grace → SIGKILL)                  |
| `ServiceSpec` 構造体  | `capsule_types::capsule_v1::ServiceSpec` (`oneshot: bool` を追加) |
