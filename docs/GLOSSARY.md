# Glossary — Ato / Capsule プロジェクト用語集

**対象読者:** 開発チームメンバー（コントリビュータ、エージェント）  
**最終更新:** 2026-04-23  
**関連:** `docs/specs/README.md`, `AGENTS.md`

---

## 目次

1. [コアコンセプト](#1-コアコンセプト)
2. [マニフェストと設定](#2-マニフェストと設定)
3. [ランタイムとルーティング](#3-ランタイムとルーティング)
4. [ライフサイクル管理](#4-ライフサイクル管理)
5. [IPC とゲストプロトコル](#5-ipc-とゲストプロトコル)
6. [アーカイブフォーマット (.sync)](#6-アーカイブフォーマット-sync)
7. [ストレージと CAS](#7-ストレージと-cas)
8. [アイデンティティと署名](#8-アイデンティティと署名)
9. [Ato Store / Playground](#9-ato-store--playground)
10. [主要型名（Rust）](#10-主要型名rust)
11. [環境変数リファレンス](#11-環境変数リファレンス)
12. [パスリファレンス](#12-パスリファレンス)
13. [廃止・移行済み用語](#13-廃止移行済み用語)

---

## 1. コアコンセプト

### Capsule（カプセル）
実行可能なアプリケーション単位。`capsule.toml` をマニフェストに持ち、ソース・Wasm・OCI・Web のいずれかのランタイムで動く。`ato` を通じてパック・配布・実行される。

### ato（アト）
ブランド名。ato-cliやato-desktopなどプロダクトのprefixとして<brand_name>-<tool_gerenal_noun>という形で命名する¥

### ato-cli
開発者が直接触る **メタ CLI**（`apps/ato-cli`）。`nacelle` を内包するのではなく、JSON over stdio で呼び出すオーケストレーターとして機能する。コマンド体系は `ato open`, `ato pack`, `ato publish` など。

### Engine（エンジン）
`ato-cli` が **ワークロード実行を委譲する外部バイナリ** の抽象概念。`nacelle` が現行の唯一の実装だが、CLI 側は特定実装に依存せず、`core/src/engine.rs::run_internal` 経由で `<engine> internal <subcommand>` を stdin JSON payload 付きで呼び出す契約のみを持つ。

- **主な責務**: ワークロード実行プロセスの spawn・監視、OS ネイティブサンドボックス隔離（Landlock/Seatbelt/eBPF）、ケイパビリティ検査（`engine features`）
- **発見順序**: `--nacelle` フラグ → `NACELLE_PATH` 環境変数 → `capsule.toml` の `[engine]` セクション → `~/.ato/config.toml` に登録されたデフォルトエンジン → portable mode（`ato` バイナリ横の `nacelle`）
- **必須度**: 全 CLI コマンドのうち `ato run` / `ato engine features` など実行系のみがエンジン必須。`ato validate` / `ato inspect` / `ato sign` / `ato publish` などビルド・配布系はエンジン無しで完結する
- **縮退動作**: エンジン未発見時、`ato run` は `auto_bootstrap_nacelle` で自動ダウンロードを試みる。失敗時は `AtoError::EngineMissing` で明示的に停止（fail-closed）

**Engine は言語ツールチェーンではない**:
エンジンは Python や JavaScript のコードを **解釈しない**。ユーザーコードを解釈・実行するのは [Provider Toolchain](#provider-toolchainプロバイダツールチェーン)（`uv`, `node`, `deno` 等）の仕事で、エンジンはその Provider Toolchain プロセスをサンドボックス内で起動・監視するだけ。階層は以下:

```
ato-cli (オーケストレータ)
  └─ Engine (nacelle) … サンドボックス + プロセス管理
       └─ Provider Toolchain (uv/node/deno) … 言語ランタイム
            └─ Workload … ユーザーコード本体
```

### Workload（ワークロード）
Capsule が実行する **ユーザーコード本体**。`capsule.toml` の `entrypoint` が指す Python スクリプト・JS サーバー・ネイティブバイナリなど、実際に「仕事をする」プロセス。エンジンは Workload プロセスを直接実装せず、Provider Toolchain を介して起動する。ログ・exit code・IPC メッセージの発生源であり、Capsule の観測対象そのもの。

### nacelle（ナセル）
**Source ランタイムエンジン**（`apps/nacelle`）の現行実装。`ato-cli` から `nacelle internal exec` として呼び出され、OS ネイティブ隔離（Landlock/Seatbelt/eBPF）を担う。ユーザーが直接叩くことはない。将来的に同じエンジン契約を満たす代替実装（WebAssembly ベース等）が追加され得るが、現時点では唯一の実装。

### Magnetic Web
Capsule エコシステム全体を指すアーキテクチャコンセプト。`did:key` による分散 ID、`.sync` による自己更新データ、`mag://` URI による位置非依存な状態参照を組み合わせる。

### Smart Build, Dumb Runtime
設計原則。**ビルド時**にマニフェスト検証・依存解決・設定の事前計算を行い（Smart）、**ランタイムは**その計算済み設定を愚直に実行する（Dumb）。nacelle はフラグ注入や推測を行わない。

---

## 2. マニフェストと設定

### `capsule.toml`
プロジェクトルートに置くプロジェクトマニフェスト。Capsule の名前・バージョン・型・ターゲット定義・サービス・ライフサイクルフックを記述する。現行 `schema_version = "0.3"` のみ受理。

### `schema_version`
`capsule.toml` のスキーマバージョン識別子。現行の `ato-cli` は `"0.3"` のみ受理する。旧形式 (`"0.2"`, `"1.0"`, `"1.1"` 等) は validation error になる。

### `default_target`
`capsule.toml` のトップレベルフィールド。`ato open` / `ato build` 時に target ラベルが省略された場合に使われる `[targets.<label>]` の名前。

### `[targets.<label>]`
ターゲット定義セクション。各ターゲットは `runtime`, `driver`, `entrypoint` を持つ。複数定義して `default_target` で切り替える。

### `runtime`
ターゲットの実行方式。許可値: `source` / `web` / `wasm` / `oci`。

### `driver`
`runtime = "source"` / `"web"` 時の実行ドライバ。許可値: `native` / `python` / `node` / `deno` / `static` / `wasmtime`。省略時はエントリポイントや `language` から推論する（推論廃止方向）。

### Provider Toolchain（プロバイダツールチェーン）
ユーザーコードを解釈・実行する **言語ランタイムまたはパッケージマネージャ** の総称。Rust 型は `ProviderToolchain` enum で、`Uv` / `Pnpm` / `Npm` / `Deno` / `Auto` などのバリアントを持つ（`--via <toolchain>` フラグで明示指定可能）。

- **Engine との違い**: [Engine](#engineエンジン) はサンドボックス + プロセス管理の担当で言語非依存。Provider Toolchain は Python バイトコードや JS を実際に走らせる言語固有ランタイム。Engine が Provider Toolchain を spawn し、Provider Toolchain が Workload を実行する 2 層構造
- **プロビジョニング**: `~/.ato/runtimes/` に JIT ダウンロードされる。Capsule ごとに必要なバージョンを隔離管理
- **例**: `driver = "python"` の Capsule は通常 `uv` (Provider Toolchain) 経由で起動される → `uv run python app.py` を Engine がサンドボックス内で実行
- **`driver` フィールドとの関係**: `driver` は実行カテゴリ（どの系統のランタイムを使うか）のみを宣言し、実体の Provider Toolchain 選定は `ato-cli` が行う

### `required_env`
起動前に存在チェックを行う必須環境変数名リスト。未設定または空文字なら **fail-closed** で停止。

### `[ipc.exports]` / `[ipc.imports]`
Capsule が公開する Capability の定義（`exports`）と、依存する他 Capsule の Capability 宣言（`imports`）。`ato-cli` (IPC Broker) がこれを読んでサービスを自動起動・注入する。

### `CapsuleManifest`（型）
`capsule.toml` をパースした Rust 型（`capsule-core/src/types/manifest.rs`）。CLI が manifest 操作に使う中心的な構造体。

---

## 3. ランタイムとルーティング

### `RuntimeKind`（enum）
`capsule-core/src/router.rs` で定義。`Source` / `Oci` / `Wasm` / `Web` の 4 値。`router::route_manifest()` が `capsule.toml` を見てどのエンジンにディスパッチするかを決める。

### `RuntimeDecision`（struct）
ルーティング結果。`kind: RuntimeKind` と実行に必要なパラメータを持つ。`router::route_manifest()` が返す。

### `ExecutionDescriptor`（struct）
ランタイムに渡す実行計画の詳細（コマンド、環境変数、ポートなど）。`RuntimeDecision` の中に内包される。

### `RunPlan`（struct）
ランタイム別の実行計画（`capsule-core/src/types/runplan.rs`）。`RunPlanRuntime` enum で `SourceRuntime` / `DockerRuntime` / `NativeRuntime` に分岐する。

### `ExecutionProfile`（enum）
`router.rs` で定義。`Supervisor` / `Single` など実行モードを表す。

### IPC Broker
**ato-cli** が担う役割。全 Capsule 間通信を仲介する。Service 解決・RefCount・Bearer Token 管理・Schema 検証を行う。nacelle は IPC Broker ではなく **Sandbox Enforcer** に専念する（CAPSULE_IPC_SPEC v1.1 の破壊的変更）。

### Sandbox Enforcer
**nacelle** が担う役割。OS レベルの隔離（Landlock/Seatbelt/eBPF）のみを担当し、IPC の内容には関与しない。

---

## 4. ライフサイクル管理

### Task（タスク）
**ワンショット実行**。`exit 0` で「完了」とみなされる。`[tasks.<name>]` で定義。内部的に `ServiceSpec { oneshot: true }` に変換される。例: `build`, `migrate`。

### Service（サービス）
**常駐プロセス**。プロセス終了は異常として fail-fast 動作する。`[services.<name>]` で定義。内部的に `ServiceSpec { oneshot: false }` に変換される。例: `httpd`, `llama-server`。

### `ServiceSpec`（struct）
nacelle の `supervisor_mode.rs` が受け取る統一実行単位（`capsule-core/src/types/manifest.rs`）。`oneshot: bool` フラグで Task/Service を区別する。

### DAG（有向非巡回グラフ）
`depends_on` で記述されたタスク/サービス依存関係のグラフ。`supervisor_mode.rs::resolve_dependencies()` がトポロジカルソートして実行レイヤー（`Vec<Vec<TaskID>>`）を生成する。

### `[lifecycle]`
`capsule.toml` のライフサイクル定義セクション。`setup`（起動前 Task）・`run`（メイン Service）・`stop`（終了 Task）・`port`・`stop_timeout` を持つ。Level 1 ショートカット記述（直接コマンド）と Level 2 参照記述（Task/Service 名）の二層構造。

### Readiness Probe
Service が「起動完了」したかを判定する仕組み。`http_get` または `tcp_connect` で設定。`readiness_probe = { http_get = "/health", port = "APP_PORT" }`。

### Layered Execution
DAG をトポロジカルソートした実行レイヤー構造。`parallel = false`（デフォルト）では逐次、`parallel = true` では同一レイヤー内を `tokio::spawn` + `JoinSet` で並列実行。

### fail-closed
安全側倒れの原則。必須環境変数が未設定・失効鍵・権限不足などの場合、黙って続行せず明示的にエラーで停止する。

---

## 5. IPC とゲストプロトコル

### Guest Mode / App-in-App
Host アプリ（ato Runtime）内で別の Capsule を実行する機能。Host が Guest を子プロセスとして spawn し、stdin/stdout で JSON-RPC 2.0 通信する。

### `GuestMode`（enum）
ゲストの UI 表示モード（`src/adapters/ipc/guest_protocol.rs`）:
- `Widget` — PiP / Floating ウィンドウ
- `Headless` — UI なし、常駐アイコンのみ
- `App`（スペック上）— タブ/ウィンドウ占有

### `GuestContextRole`（enum）
データアクセス権限の役割:
- `Consumer` — `.sync` の Payload を **読み取り専用** で利用
- `Owner` — **書き戻し**（Write-back）権限あり

### `GuestAction`（enum）
Guest が Host に対して要求できる操作。stdio 上では JSON-RPC 2.0 method 名と
`guest.v1` envelope の双方から同じ `dispatch_guest_action` に解決される（envelope
auto-detect — 詳細は `DRAFT_CAPSULE_IPC.md`）:
- `ReadPayload` / `WritePayload` / `UpdatePayload` — `.sync` ペイロードの読み書き
- `ReadContext` / `WriteContext` — コンテキスト JSON の読み書き
- `ExecuteWasm` — sync.wasm の実行（JSON-RPC 経路では `capsule/wasm.execute` を将来用に予約済み、現状 `-32601`）

### `GuestRequest` / `GuestResponse`（struct）
旧 `guest.v1` envelope のメッセージ型（compat lane）。`GuestRequest` は
`version`, `request_id`, `action`, `context`, `input` を持つ。新規実装は
JSON-RPC 2.0 経路を使うこと。

### JSON-RPC 2.0
IPC の統一メッセージフォーマット（Phase 13b.9 で完了 2026-04-29）。Guest stdio で
受け付ける method 名空間:
- `capsule/payload.{read,write,update}` — `.sync` ペイロード操作
- `capsule/context.{read,write}` — コンテキスト操作
- `capsule/wasm.execute` — 予約（WASM/OCI 整備後に公開）

`capsule/invoke` は Service-to-Service 用に予約されており（params shape:
`{ service, method, token, args }`）、Host-to-Guest stdio では使用しない。

### NDJSON（Newline Delimited JSON）
nacelle の stdout フォーミングに使われるプロトコル。1行1メッセージ。

### Transport
IPC の通信路。優先順位: `stdio`（Host-Guest）→ `Unix Domain Socket`（Local Service）→ `Named Pipe`（Windows）→ `tsnet`（Remote）→ `HTTP/WebSocket`（Fallback）。

### `mag://` URI
`mag://<DID>:<SchemaHash>/<MerkleRoot>/<Path>` の形式の URI。場所（Location）ではなく状態（State）を指す。DID 解決 → Schema 検証 → Capsule 選択 → データ取得の順で解決される（`MAG_URI.md`）。

---

## 6. アーカイブフォーマット (.sync)

### `.sync`
**Self-Updating Data Archive** の統一規格。拡張子 `.sync`、実体は Standard ZIP（全エントリ Stored/無圧縮）。MIME タイプ `application/vnd.magnetic.sync+zip`。

### `manifest.toml`（.sync 内）
`.sync` ファイル内のメタデータファイル。`[sync]`, `[meta]`, `[policy]` が必須セクション。`[encryption]`, `[permissions]`, `[signature]` は省略可能。

### `payload`
`.sync` ZIP 内の単一ファイルエントリ。復号後のコンテンツ本体。Stored（無圧縮）必須。複数ファイルは TAR でまとめて単一 payload にする。

### `sync.wasm`
`.sync` ZIP 内の Wasm モジュール。自己更新ロジックを担う（例: license.sync のサブスクリプション更新）。

### `context.json`
`.sync` ZIP 内の任意ファイル。実行コンテキスト情報を持つ JSON。

### `sync.proof`
`.sync` ZIP 内の任意ファイル。ZK 証明（STARK 等）の格納先。

### Vault Mode
`.sync` の暗号化バリアント。`[encryption] enabled = true, algorithm = "age-v1"` で有効化。`variant = "vault"` で識別。

### `profile.sync`
ユーザーのパブリックプロファイルを `.sync` カプセルとして配布するフォーマット。`content_type = "application/vnd.capsule.profile"`。

### `license.sync`
ライセンス（利用権）を `.sync` カプセルとして発行するフォーマット。`content_type = "application/vnd.capsule.license"`。`sync.wasm` でサブスクリプション期限の自己更新が可能。

---

## 7. ストレージと CAS

### CAS（Content-Addressable Storage）
`capsule-core/src/capsule_v3/cas_store.rs` が実装するコンテンツアドレス指向ストレージ。ファイルを BLAKE3 ハッシュで識別・重複排除する。`~/.ato/cas/` に保存。

### `CapsuleManifestV3`（struct）
CAS ベースのパック形式（V3）のマニフェスト型（`capsule-core/src/capsule_v3/manifest.rs`）。チャンクリスト、CDC パラメータ、アーティファクトハッシュを持つ。

### `CdcParams`（struct）
FastCDC（Content-Defined Chunking）のパラメータ。チャンク境界決定に使う。

### `ChunkMeta`（struct）
各チャンクのメタデータ。BLAKE3 ハッシュ、オフセット、サイズを持つ。

### JCS（JSON Canonicalization Scheme, RFC 8785）
署名対象 JSON を正規化するアルゴリズム。プロパティ順・空白を決定論的にする。`compute_artifact_hash_jcs_blake3()` で使用。

### BLAKE3
ハッシュアルゴリズム。Capsule のアーティファクトハッシュ、payload ハッシュ、チャンクハッシュに使用。SHA-256 より高速。

---

## 8. アイデンティティと署名

### `did:key`
分散 ID の一形式（W3C DID Core 準拠）。中央認証局なしで生成・検証できる。フォーマット: `did:key:z6Mk<multibase-encoded-ed25519-public-key>`。

### Ed25519
非対称署名アルゴリズム。Capsule 署名・プロファイル署名・ライセンス署名に使用。

### TOFU（Trust On First Use）
初回接続時に Fingerprint を記録し、次回以降の差異を警告する信頼モデル。`~/.ato/trust_store.json` に保存。

### Trust Store
ローカルの信頼データベース。鍵フィンガープリント・Petnames・失効リストキャッシュを管理。

### Petname（ペットネーム）
ユーザーが任意に付与できる人間可読のエイリアス。`did:key` の長い文字列を「Alice」のように参照可能にする。`~/.ato/petnames.json` に保存。

### Trust State
- **Verified** — 署名一致 + 失効なし
- **Untrusted** — 署名不一致 / 失効済み
- **Unknown** — 未検証（ネットワーク未到達）

### Developer Key
Capsule 署名・アプリ公開に使う鍵。`~/.ato/keys/<name>.json` に格納（`0600` パーミッション）。`ato-cli` が管理。

### User Key
ライセンス所有証明・P2P 認証に使う鍵。OS Keychain に格納。`ato-desktop` が管理。

### Device Key
デバイス間同期・Tailnet 認証に使う鍵。Keychain + `ato-tsnetd` が管理。

### PoL（Proof of License）
ライセンス所有を暗号学的に証明する仕組み。`license.sync` の署名を `grantee`（購入者 DID）と照合して検証する。

---

## 9. Ato Store / Playground

### Ato Store
Capsule の検索・配布・販売を行うパッケージレジストリ。`apps/ato-store`（Cloudflare Workers + Hono + D1 + R2）。

### Publisher
Capsule を Ato Store に公開する開発者。`did:key` と OAuth を紐づけた Publisher Identity Model を持つ。

### Playground
`play.ato.run` で提供される Capsule の実行環境。Trust Origin（`play.ato.run`）と Untrusted Origin（`*.atousercontent.com`）を iframe で分離。`apps/ato-play-web` / `apps/ato-play-edge`。

### Theater UI
Playground の実行画面。`/:slug` で特定 Capsule を表示する。

### Launchpad
Playground のトップページ。`Continue Building` / `Made for You` / `Fresh & Trending` の 3 行で Capsule を推薦する。

### TVM（Token Verification Middleware）
`ato-proxy-edge` が担う JWT 検証・API キー差し替え・OpenAI リレーの仕組み。`proxy.ato.run` で動作。

---

## 10. 主要型名（Rust）

| 型名 | ファイル | 説明 |
|------|---------|------|
| `CapsuleManifest` | `core/src/types/manifest.rs` | `capsule.toml` パース結果 |
| `CapsuleType` | `core/src/types/manifest.rs` | `app` / `tool` / `inference` |
| `RuntimeType` | `core/src/types/manifest.rs` | マニフェスト上の runtime 列挙 |
| `RuntimeKind` | `core/src/router.rs` | ルーター用 `Source/Oci/Wasm/Web` |
| `RuntimeDecision` | `core/src/router.rs` | ルーティング結果（kind + 実行パラメータ） |
| `ExecutionDescriptor` | `core/src/router.rs` | 実行計画詳細 |
| `ExecutionProfile` | `core/src/router.rs` | `Supervisor` / `Single` など |
| `RunPlan` | `core/src/types/runplan.rs` | ランタイム別実行計画 |
| `ServiceSpec` | `core/src/types/manifest.rs` | Task/Service 統一実行単位（`oneshot: bool`） |
| `ReadinessProbe` | `core/src/types/manifest.rs` | 起動完了判定設定 |
| `IsolationConfig` | `core/src/types/manifest.rs` | `allow_env` などの隔離設定 |
| `NetworkConfig` | `core/src/types/manifest.rs` | `egress_allow` などのネットワーク制御 |
| `CapsuleManifestV3` | `core/src/capsule_v3/manifest.rs` | CAS パック形式マニフェスト |
| `CasStore` | `core/src/capsule_v3/cas_store.rs` | コンテンツアドレス指向ストレージ |
| `GuestMode` | `src/adapters/ipc/guest_protocol.rs` | `Widget` / `Headless` |
| `GuestContextRole` | `src/adapters/ipc/guest_protocol.rs` | `Consumer` / `Owner` |
| `GuestAction` | `src/adapters/ipc/guest_protocol.rs` | `ReadPayload` / `WritePayload` / ... |
| `GuestRequest` | `src/adapters/ipc/guest_protocol.rs` | IPC リクエストメッセージ |
| `GuestResponse` | `src/adapters/ipc/guest_protocol.rs` | IPC レスポンスメッセージ |
| `GuestPermission` | `src/adapters/ipc/guest_protocol.rs` | Guest の権限セット |
| `IpcBroker` | `src/ipc/broker.rs` | IPC ブローカー（ato-cli 実装） |
| `IpcTransport` | `src/ipc/types.rs` | Transport 種別 |
| `CapsuleError` | `core/src/types/error.rs` | Library エラー型（thiserror） |
| `ProfileManifest` | `core/src/types/profile.rs` | `profile.sync` のマニフェスト型 |
| `SchemaRegistry` | `core/src/schema_registry.rs` | Capability スキーマ管理 |

---

## 11. 環境変数リファレンス

### Guest プロトコル（Host → Guest 注入）

Phase 13b.9 (2026-04-29) で `CAPSULE_IPC_*` 名前空間に統一済み。旧 `GUEST_*` /
`CAPSULE_GUEST_*` 名は削除（fallback なし）。

| 変数名 | 値例 | 説明 |
|--------|------|------|
| `CAPSULE_IPC_PROTOCOL` | `jsonrpc-2.0` / `guest.v1` | Guest stdio envelope の優先プロトコル（auto-detect でも上書き可） |
| `CAPSULE_IPC_TRANSPORT` | `stdio` | 現状 `stdio` のみ |
| `CAPSULE_IPC_MODE` | `headless` / `widget` / `app` | 起動時の UI モード |
| `CAPSULE_IPC_ROLE` | `consumer` / `owner` | データアクセス役割（`GuestContextRole` と一致） |
| `CAPSULE_IPC_SYNC_PATH` | `/path/to/data.sync` | 対象 `.sync` ファイルのパス（WASI mount path `SYNC_PATH=/sync` は別レイヤーで継続） |
| `CAPSULE_IPC_WIDGET_BOUNDS` | `120x120+0+0` | widget モードの初期サイズ・位置 |
| `ALLOW_HOSTS` | `api.example.com,...` | 許可 egress ホスト一覧 |
| `ALLOW_ENV` | `API_KEY,...` | Guest に透過する環境変数名一覧 |

### IPC（ato-cli → Capsule 注入）

| 変数名 | 値例 | 説明 |
|--------|------|------|
| `CAPSULE_IPC_<SERVICE>_URL` | `unix:///tmp/capsule-ipc/llm.sock` / `http://localhost:34567` | Service への接続 URL |
| `CAPSULE_IPC_<SERVICE>_TOKEN` | `tok_abc123` | Service 認証 Bearer Token |
| `CAPSULE_IPC_<SERVICE>_SOCKET` | `/tmp/capsule-ipc/llm.sock` | Unix ドメインソケットの実パス（`unix://` URL のみ） |

### 実行環境

| 変数名 | 値例 | 説明 |
|--------|------|------|
| `NACELLE_PATH` | `/path/to/nacelle` | nacelle バイナリのパスオーバーライド |
| `ATO_TOKEN` | `<token>` | ato-cli の認証トークン（keyring より優先） |
| `RUST_BACKTRACE` | `1` | Rust バックトレース有効化（デバッグ用） |

---

## 12. パスリファレンス

| パス | 説明 |
|------|------|
| `~/.ato/` | ato-cli のデータルートディレクトリ（`~/.capsule/` から移行済み） |
| `~/.ato/config.toml` | CLI 設定ファイル |
| `~/.ato/store/` | インストール済み Capsule の格納先 |
| `~/.ato/keys/` | Developer Key 格納先（`<name>.json`、`0600`） |
| `~/.ato/cas/` | CAS チャンクストレージ |
| `~/.ato/runtimes/` | JIT Provisioning されたランタイムバイナリ |
| `~/.ato/trust_store.json` | TOFU フィンガープリント記録 |
| `~/.ato/petnames.json` | Petname マッピング |
| `capsule.toml` | プロジェクトルートのマニフェスト |
| `capsule.lock.json` | マニフェストのロックファイル（`ato validate` が検証） |
| `apps/uarc/schemas/capsule.schema.json` | `capsule.toml` の JSON Schema 定義（Single Source of Truth） |
| `docs/specs/` | アーキテクチャ仕様書群 |
| `samples/` | サンプルアプリ |

---

## 13. 廃止・移行済み用語

| 旧用語 / 旧フィールド | 現行 | 備考 |
|----------------------|------|------|
| `~/.capsule/` | `~/.ato/` | Phase PR1 で完全移行済み（2026-03） |
| `[execution]` セクション | `[targets.<label>]` + `[lifecycle]` | parse error として拒否される |
| `targets.source` / `targets.wasm` / `targets.oci` | `[targets.<label>] runtime = "..."` | named target 前提に変更済み |
| `runtime = "web", driver = "browser_static"` | `driver = "static"` | 廃止済み、指定すると validation error |
| `public` フィールド（web target） | 廃止 | validation error |
| `auto_install_deps` | `[lifecycle] setup = "..."` | 段階的廃止中（Phase 2 で deprecation 警告予定） |
| `ato-coordinator` (Go Registry) | `ato-store` (Cloudflare Workers) | 完全置換済み |
| `guest.v1` envelope 単独 | JSON-RPC 2.0 (`capsule/payload.*` / `capsule/context.*`) + `guest.v1` 互換レーン | Phase 13b.9 完了（2026-04-29）— envelope auto-detect で両方受理 |
| `CAPSULE_GUEST_PROTOCOL` / `GUEST_MODE` / `GUEST_ROLE` / `GUEST_WIDGET_BOUNDS` | `CAPSULE_IPC_PROTOCOL` / `_MODE` / `_ROLE` / `_WIDGET_BOUNDS` | Phase 13b.9 で削除（fallback なし） |
| `spec_version = "0.1.0"` | `spec_version = "1.0"` | nacelle の legacy 互換のみ受理 |
| `ato-desktop` (Tauri 版) | `ato-desktop` (GPUI + Wry 版) | アーキテクチャを完全刷新（Zed 型シェル） |
| macOS sandbox: 静的 `sandbox-exec` predefined profile | 動的 SBPL (`sandbox_init(flags=0)`) | Phase 13a 完了（2026-04-29、ADR-007） |

---

*このドキュメントは `docs/specs/` 内の各仕様書と `apps/` 以下のソースコードから生成しています。仕様変更時は各 Spec ファイルと合わせて更新してください。*
