---
title: "Ato CLI Spec (v0.3)"
status: accepted
date: "2026-02-17"
author: "@egamikohsuke"
ssot:
  - "apps/ato-cli/"
related:
  - "CAPSULE_CORE.md"
  - "NACELLE_SPEC.md"
---

# Ato CLI Spec (ato-cli)

## 1. 概要

- `ato` はメタ CLI として `capsule.toml` を読み、下位エンジン（nacelle / Docker / wasmtime）へディスパッチ。
- Engine とは JSON over stdio のプロセス境界で連携。
- **IPC Broker** として、Service 解決・参照カウント・Token 管理・Schema 検証を担う。全ランタイム (Source/OCI/Wasm) の上位に位置するため、ランタイム横断で IPC を統括できる唯一のレイヤー。

## 2. 目的 / スコープ

- **目的:** Capsule の作成/実行/パッケージ/署名を CLI で提供。**IPC Broker** として Capsule 間通信のオーケストレーションを行う。
- **スコープ内:**
  - ランタイムルーティング (`router.rs`: Source/Web/OCI/Wasm)
  - IPC Broker (Service 解決, RefCount, Token 管理, Schema 検証, DAG 統合)
  - IPC 診断コマンド (`ato ipc status` 等)
  - Guest プロトコル (stdio JSON-RPC)
- **非スコープ:** Engine 内部実装、GUI/UX、OS レベル隔離（各 Engine の責務）。

### 2.1 Preview Promotion Modes

GitHub public repo の zero-config 実行は、単一の permissive mode ではなく次の 3 mode に分離する。

| Mode | 目的 | 許可する補完 | fail-closed のまま残すもの | 保存先 |
| --- | --- | --- | --- | --- |
| Preview | public repo を非侵襲に評価し、実行可能性を確認する | `runtime_version` の遅延評価、`runtime=web` の `port` 遅延評価、lock/sandbox 強制の一部遅延、generated `capsule.toml` | 危険な runtime/driver 組み合わせ、unsafe path、entrypoint 安全性、required env 未設定の実行、publish 相当の厳格条件 | `~/.ato/previews/<preview_id>/metadata.json`, `~/.ato/previews/<preview_id>/capsule.toml` |
| Promotion | successful preview の結果を immutable install object として固定化する | preview 由来 metadata の snapshot 化、promotion provenance の固定化 | content hash 不一致、artifact 改ざん、install object の上書き破壊 | store install dir の `promotion.json`, runtime tree の `promoted/` namespace |
| Author Build / Publish | author 管理下 repo の fail-closed build/publish | 補完なし | lock、`runtime_version`、`port`、`pack.include`、publish preflight 全般 | project root, store artifact, CI/publish outputs |

Preview は author intent を書き換えない derived state として扱う。checkout の移動や invocation directory 配下を正本化してはならない。

### 2.2 LockDraft / Local Finalize

- preview / store / store-web が共有する生成対象は completed `ato.lock.json` ではなく `LockDraft` とする。
- `LockDraft` は少なくとも `Draft`, `ReadyToFinalize`, `FinalizedLocally`, `PreviewVerified`, `Publishable` の状態を持つ。
- server / web が返してよいのは `lockDraft`, `draft_hash`, readiness, remediation command, warning までであり、canonical な `ato.lock.json` の write は local CLI のみが担当する。
- shared LockDraft engine は pure function として native / WASM の両方へビルドし、filesystem access / network access / env / clock / cache は host adapter 側へ隔離する。

## 3. コマンド

### 3.1 Primary Commands

CLI の front-door は `run`, `decap`, `encap` の 3 つに固定する。物語は `Try -> Keep -> Share` とし、他コマンドは互換・高度機能として help 上の優先度を下げる。

- `ato run [path|publisher/slug|github.com/owner/repo|https://ato.run/s/<id>] [-t <label>] [--entry <id>] [--env-file <path>] [--prompt-env] [--watch] [--background] [--nacelle] [--registry] [--enforcement] [-y|--yes]`
  - 既定で `path="."`。`.capsule` または `capsule.toml` を実行。
  - `run` は常に ephemeral execution とする。repo や workspace を永続配置してはならない。
  - `capsule://...` canonical handle は Phase 1 では拒否する。CLI の run surface は terse ref と local path のみに保つ。
  - `github.com/owner/repo` 入力は PreviewSession を内部生成し、`preview metadata` → optional retry/manual edit → build → promotion → installed run の順で進む。
  - share URL (`https://ato.run/s/<id>` / `https://ato.run/s/<id>@r<revision>`) は first-class input として受理し、mutable URL は内部で immutable revision に固定してから run / next-step を組み立てる。
  - workspace target を run する場合、resolver は runnable `entries[]` を返し、`run` はその中から 1 つを ephemeral 実行する。
    - runnable entry が 1 つなら自動選択
    - 複数なら TTY では対話選択
    - non-TTY では `--entry <id>` を必須とする
  - required env がある target は process failure まで進めず preflight で停止する。入力手段は `--env-file`, `--prompt-env`, target-local saved env reuse (`~/.ato/env/targets/<fingerprint>.env`) の 3 つを正本とする。
  - `run` は repo 配下や target workspace 配下に `.env` を自動生成しない。
  - MVP では独立した `ato preview` コマンドは持たず、`ato run github.com/...` の内部フローとして preview を実行する。
  - 実行前ガード（RuntimeGuard）:
    - `source/node` は Tier1 として扱い、`--unsafe` は不要。
    - `source/native` / `source/python` は `--unsafe` と `--enforcement strict` が必須。
    - `source/node` の lock 要件は `package-lock.json` のみ（`pnpm-lock.yaml` / `yarn.lock` は非許容）。
    - consumer 実行経路は Hermetic Runtime を強制し、システムの `deno/python/uv` にはフォールバックしない。
    - 必須ランタイム/ツールの解決元は `capsule.lock.json` の `runtimes` / `tools`（`url + sha256`）のみ。
  - 解決順序は次を正本とする:
    1. `./` `../` `~/` `/`（Windows は `C:\` など）で始まる入力はローカルパスとして解決
    2. `github.com/owner/repo` 形式は GitHub public repo として preview session 作成 → optional retry/manual edit → build → promotion → install object run に解決
    3. `http(s)://github.com/...` や `www.github.com/...` は自動採用せず、`github.com/owner/repo` の canonical 形式へ誘導する
    4. それ以外で `publisher/slug`（または `@publisher/slug`）形式は Store 参照として解決
    5. `slug` 単独は拒否し、`scoped_id_required` と候補提示（Did-you-mean）を返す
  - `--registry` 未指定時は `https://api.ato.run` を使って Store API に接続する。
  - `-y/--yes` 指定時は確認を省略して install → run を継続する。
  - 非TTY（CI等）で未インストールかつ `-y/--yes` 未指定の場合は、ハング回避のためエラー終了する。
  - GitHub preview の stop points は次の通り。
    1. generated `capsule.toml` preview 表示後
    2. smoke failure 後の retry draft 適用前
    3. manual intervention が必要と判断された時点
    4. successful build 後、promotion 済み install object を run する直前
- `ato decap <target> --into <dir> [--plan]`
  - `decap` は persistent local setup に固定する。
  - `run` と同じ resolver を使うが、intent は workspace materialization とする。
  - `target` は少なくとも `github.com/owner/repo`, `publisher/slug`, share URL, local `share.spec.json`, local `share.lock.json` を受理する。
  - share URL と GitHub repo は immutable revision / pinned source を正本にする。
  - `--into` は必須で、非空 directory への暗黙上書きは拒否する。
  - 実行後は `.ato/share/state.json` を残し、人間向け summary は local setup 継続に寄せる。
- `ato encap [path] [--internal|--private|--local] [--print-plan]`
  - `path` は省略可。省略時はカレントディレクトリを対象にする。
  - デフォルト動作は public URL 生成付きアップロード (`"unlisted"` スコープ)。
  - `--internal`: 組織内限定 visibility でアップロード。`--private`: 認証済みオーナーのみ。`--local`: ローカル保存のみ (アップロードなし)。
  - `--internal` / `--private` / `--local` は相互排他。
  - `encap` は current workspace を観測し、確認済み setup 情報を share descriptor に落とす。
  - source of truth は `share.spec.json` であり、`guide.md` は説明レイヤに留める。
  - capture は `observed -> confirm -> save/share` を強制し、high-confidence 推定でも無確認 publish はしない。

#### 内部実装注記 — Narrative と Pipeline の分離

`Try / Keep / Share` は **narrative 階層**での分類であり、内部 pipeline の実装構造とは独立している。

現在の実装では `encap`/`decap` は `HourglassFlow` の外に置かれているが、v0.5.x では以下の variant を追加し、§14 エラー分類・rollback 機構・progress UI・capability gate を共通化する計画がある:

| variant | 対応コマンド | 含む stage | 含まない stage |
|---|---|---|---|
| `WorkspaceMaterialize` | `decap` | Install, Verify | Build, Execute, Publish |
| `WorkspaceCapture` | `encap` (local capture) | Prepare, Verify | Install, Execute, Publish |

この変更のトリガー: §04 sandbox network enforcement を hourglass Verify に追加する時点で、同じ enforcement ロジックを `decap` 側にも書く重複が顕在化するため。

**v0.5 (現在)**: 現状の分離実装を維持。本注記が設計意図の単一ソース。  
**v0.5.x**: `WorkspaceMaterialize` / `WorkspaceCapture` を切り出し、enforcement を 1 箇所に集約。

- `ato resolve <ref|capsule://...> [-t <label>] [--registry] [--json]`
  - debugging / automation / desktop control-plane 用の解決 surface。
  - `publisher/slug`, `github.com/owner/repo`, local path, canonical `capsule://...` を受理する。
  - loopback registry canonical handle (`capsule://localhost:<port>/publisher/slug`, `capsule://127.0.0.1:<port>/...`, `capsule://[::1]:<port>/...`) を受理する。
  - 返却 payload は少なくとも `canonical_handle`, `source`, `trust_state`, `restricted`, `snapshot`, `launch_plan` を含み、handle identity と resolved snapshot identity を分離して表現する。
  - `ato://...` host route は capsule handle としては解決せず reject する。
- `ato install <publisher/slug> [--registry] [--version] [--default] [--skip-verify] [--output] [--json]`
  - `@publisher/slug` は入力互換として受理し、内部で `publisher/slug` に正規化する。
  - `slug` 単独は拒否する（特権グローバル名なし）。
  - `--from-gh-repo github.com/owner/repo` は後方互換の補助導線として維持する。
  - `--skip-verify` は互換フラグとして受理するが、実行時は常に拒否する（検証スキップ不可）。
  - GitHub preview 由来 artifact を install した場合、install dir に `promotion.json` を保存し、runtime tree は `promoted/` namespace を優先する。
- `ato init <name> --template <python|node|hono|rust|go|shell>`
- `ato build [dir] [--init] [--key] [--standalone] [--enforcement] [--force-large-payload]`
  - author build は strict mode を正本とし、preview 専用の permissive logic を共有しすぎてはならない。
- `ato search [query] [--category] [--limit] [--cursor] [--registry] [--json] [--no-tui]`
  - 既定の Store API は `https://api.ato.run`（`ATO_STORE_API_URL` / `--registry` で上書き可能）。

### 3.2 Management Commands

- `ato ps [--all] [--json]`
- `ato stop [--id|--name] [--all] [--force]`
- `ato logs [--id|--name] [--follow] [--tail]`

### 3.3 Auth Commands

- `ato login [--token <token>]`
  - 既定は Device Flow（`store.ato.run/auth` 経由）でブラウザ認証し、CLIへセッションを自動引き継ぐ。
  - `--token` は legacy fallback として維持。
- `ato logout`
- `ato whoami`

### 3.4 Advanced Commands

- `ato key gen [--out] [--force] [--json]`
- `ato key sign <target> --key <path> [--out]`
- `ato key verify <target> [--sig] [--signer] [--json]`
- `ato config engine features`
- `ato config engine register --name <name> --path <path> [--default]`
- `ato config engine install [--engine nacelle] [--version] [--skip-verify]`
- `ato config registry resolve <domain> [--json]`
- `ato config registry list [--json]`
- `ato config registry clear-cache`
- `ato registry serve [--port <u16>] [--data-dir <path>] [--host <ip>]`
- `ato config source register <repo_url> [--registry] [--channel stable|beta] [--installation-id] [--json]`
- `ato publish [--registry <url>] [--artifact <path>] [--ci | --dry-run] [--no-tui] [--force-large-payload] [--json]`
- `ato gen-ci`
  - `publish` のターゲット解決順は次を正本とする:
    1. `--ci` / `--dry-run`
    2. `--registry <url>`
    3. `capsule.toml` の `store.registry`
    4. `ato login` で保存された Personal Dock (`https://store.ato.run/d/<handle>`)
    5. どれも解決できない場合はエラーにし、`ato login` または `--registry https://api.ato.run` / `--ci` を案内する
  - 公式 registry（`api.ato.run` / `staging.api.ato.run`）は CI-first:
    - `publish --ci`: GitHub Actions 上で OIDC を使って `/v1/publish/ci` へ multipart publish（`did_signature` は keyless ephemeral Ed25519 で自動生成）
    - `publish --dry-run`: ローカル検証（アップロードなし）
    - `publish` (公式 registry + TTY, `--json` なし, `--no-tui` なし): GitOps オーケストレーション TUI を起動
      - `main` ブランチのみ許可
      - clean working tree 必須
      - `capsule.toml` の version を `Patch/Minor/Major` から選択して更新
      - `git commit` / `git tag` / `git push main` / `git push tag` を自動実行
      - GitHub Actions 実行完了まで監視（20分タイムアウト）
      - push 前失敗時のみローカル tag/commit を自動ロールバック
    - `publish --no-tui` または非TTY: CI-first ガイダンスを表示
  - Dock registry（`.../d/<handle>`）と非公式 custom/private registry では direct publish を実行する
    - `--artifact` 指定時は再パックせず既存 `.capsule` をアップロード
    - Dock registry では publisher を常に `<handle>` に固定し、`--scoped-id` 未指定時は `<handle>/<slug>` を自動採用する
    - `--scoped-id` を明示した場合、Dock handle と publisher が不一致ならアップロード前に fail する
    - `ato login` 済みの既定 Dock publish では保存済み credentials の publisher 情報を優先し、欠けていれば `/v1/publishers/me` で補完する
    - session token が存在する場合は `Authorization: Bearer <token>` を付与する
    - managed Store direct-upload path を使う publish には current conservative preflight limit 95MB を適用する。これは remote acceptance guarantee ではなく presigned upload 対応までの暫定 fail-fast policy である
    - managed Store direct-upload path では `--force-large-payload` と `--paid-large-payload` は無効。custom/private direct registry では従来どおり有効
    - managed Store direct-upload path で conservative limit 超過、override flag 使用、または remote `413 Payload Too Large` が起きた場合、CLI は `E212` を返す
    - CLI は upload strategy abstraction を持ち、selector の優先順位は `environment override` -> `registry capability discovery` -> `host fallback` とする。current managed Store server-advertised default は `presigned`
    - presigned strategy は `GET /v1/capsules/by/:publisher/:slug` で capsule id を解決し、必要なら `POST /v1/capsules` で bootstrap し、その後 `POST /v1/capsules/:id/releases` -> `PUT <upload_url>` -> `POST /v1/capsules/:id/releases/:version/finalize` の順で publish する
    - presigned strategy の `PUT <upload_url>` には `Authorization` header を付与しない
    - managed Store は `GET /v1/publish/capabilities` で upload strategy の current contract を advertise する
    - managed Store presigned publish は `allow_existing` overwrite flow を受け付け、registered publisher であれば verified でなくても unlisted/unverified metadata として publish できる
    - managed Store direct upload は rollback/debug 用の explicit path としてのみ残し、custom/private registry の direct upload とは位置づけを分ける
  - `gen-ci` は `.github/workflows/ato-publish.yml` を固定テンプレートで生成/更新する。
    - `push.tags: v*.*.*` + `workflow_dispatch`
    - Ato CLI バージョン固定 + SHA256 検証
    - `id-token: write` + `ato publish --ci`（key secret 不要）

### 3.5 Hidden Legacy Aliases

後方互換のため、以下は help から隠して受理する（内部処理は新コマンドと同一）。

- `open` → `run`
- `new` → `init`
- `pack` → `build`
- `close` → `stop`
- `auth` → `whoami`
- `keygen` / `sign` / `verify` → `key gen` / `key sign` / `key verify`
- `setup` / `engine` / `registry` / `source` → `config ...`
- `package search` → `search`
- `profile` / `guest` / `ipc` / `scaffold` は内部・互換用途として hidden のまま維持

### 3.6 Capsule Handle Surface

- canonical handle は `capsule://...` のみとする。
- canonical forms:
  - `capsule://github.com/<owner>/<repo>`
  - `capsule://ato.run/<publisher>/<slug>[@version]`
- loopback-only canonical forms:
  - `capsule://localhost:<port>/<publisher>/<slug>[@version]`
  - `capsule://127.0.0.1:<port>/<publisher>/<slug>[@version]`
  - `capsule://[::1]:<port>/<publisher>/<slug>[@version]`
- `capsule://<publisher>/<slug>` は invalid。
- `capsule://local/...` は invalid。
- `ato://...` は host route 専用であり、registry shorthand には使わない。
- `ato app resolve` / `ato app session start` は desktop control-plane 用に維持するが、内部実装は `capsule-core` の shared resolver を正本とする。

### 3.7 Desktop Session Contract

- `ato app resolve` は metadata-only の事前解決 surface であり、canonical handle, source, trust, restricted, snapshot candidate, runtime summary, display hint を返す。
- `ato app session start` は Desktop 実行の唯一の権威とし、materialize / boot 後の concrete session envelope を返す。
- session envelope は common fields を必須とする。
  - `session_id`
  - `handle`
  - `canonical_handle`
  - `source`
  - `trust_state`
  - `restricted`
  - `resolved_snapshot`
  - `runtime`
  - `display_strategy`
  - `notes`
- `display_strategy` は少なくとも次を持つ。
  - `guest_webview`
  - `web_url`
  - `terminal_stream`
  - `service_background`
  - `unsupported`
- `runtime=web` は `[metadata.ato_desktop_guest]` を要求しない。CLI は web runtime を起動し、Desktop が attach できる `local_url` を session payload に入れる。
- loopback registry handle でも session contract は同一で、trust は `untrusted`、isolation は fail-closed を維持する。

## 4. 設定 / ディスパッチ

- Engine パス優先順位:
  1. `--nacelle` / `NACELLE_PATH`
  2. 登録済み engine（~/.capsule/config.toml）
- Store 接続先:
  - `ATO_STORE_API_URL` (default: `https://api.ato.run`)
  - `ATO_STORE_SITE_URL` (default: `https://store.ato.run`)
  - `ATO_SESSION_TOKEN`（`CAPSULE_SESSION_TOKEN` 互換）
- JSON 出力: `--json`

## 5. パッケージング

- `build` / `isolation` を `capsule.toml` から参照
- `capsule.toml` の `[pack]` をサポート
  - `pack.include`（任意）: 指定時は strict opt-in（マッチしたファイルのみ梱包）
  - `pack.exclude`（任意）: include/通常収集後に除外
- `[pack]` 未指定時は Smart Defaults を強制適用（例: `.git`, `node_modules`, `.venv`, `__pycache__`, `.next/cache`, `.turbo`, `*.capsule` など）
- payload サイズ上限は 200MB（build/publish 共通）
  - 超過時は fail-closed
  - 明示 override は `--force-large-payload` のみ
- `targets.<label>.runtime_version`:
  - `runtime=source` かつ `driver in {deno,node,python}`、または `runtime=web` かつ `driver=deno` の場合、LockDraft engine は required runtime version を返す。
  - manifest に pin がない場合でも shared default から draft を組み立ててよいが、authoring では明示 pin を推奨する。
  - canonical な `ato.lock.json` の生成は local finalize 時にのみ行う。
  - `capsule.lock.json` 生成時に `runtimes.deno` / `runtimes.node` / `runtimes.python` が固定化される。
  - deno orchestrator で `runtime_tools.python` が指定される場合、`tools.uv` も lock に固定化される。

### 5.1 Preview-Specific Relaxations

- Preview mode では parse-time 基本整合性は維持しつつ、次の条件だけを遅延評価できる。
  - `runtime=source` かつ `driver in {deno,node,python}` の `runtime_version`
  - `runtime=web` の `port`
  - source/web の lockfile 不足
  - source/native, source/python, web/python の sandbox opt-in 要件
- Preview は canonical lock を生成しない。shared engine による `LockDraft` と remediation を返し、local CLI の finalize を待つ。
- ただし次は preview でも fail-closed のまま維持する。
  - invalid runtime/driver pair
  - unsafe relative path / shell command injection 的 entrypoint
  - `driver` 不在や unsupported driver
  - required env を満たさないままの実行
  - publish / author build の strict preflight

## 6. セキュリティ

- 署名検証は CLI 側で制御（署名キー生成/適用）。
- 実行ポリシーの意思決定（許可/拒否・同意判定・hash 計算）は CLI 側で実施し、engine には適用対象ポリシーのみを渡す。
- **IPC:** 現状は `ato build` / `ato run` の preflight で `remote = true` Capability の意図せぬ公開を警告する。

> 注記: 実行隔離契約の正本は `EXECUTIONPLAN_ISOLATION_SPEC.md` とし、
> `EXECUTIONPLAN_ISOLATION_MODEL.md` は解説ドキュメントとして扱う。

### 6.1 Diagnostics（エラー診断）

- `ato build` / `ato run` は Manifest 起因エラーと build 起因エラーを診断コード付きで表示する。
- Human 向け出力では `miette` を用いたリッチな診断表示を利用する。
- `--json` 指定時は `miette` の fancy 出力を無効化し、安定 JSON envelope を標準出力へ 1 オブジェクト出力する。
  - `schema_version`, `type=error`, `code`, `message`, `hint`, `path`, `field`, `causes`
- 診断コード（例: `E001`, `E101`）は表示専用であり、プロセス終了コードとは独立して扱う。
- `ato publish` は registry により動作を分岐する。
  - 公式 registry (`api.ato.run`, `staging.api.ato.run`): CI-first の 3 mode（`--ci` / `--dry-run` / TTY TUI）
  - 非公式 registry: private direct publish（ローカル artifact のアップロード）
- `ato search` は TTY かつ `--json` なしで対話TUIを起動し、`--no-tui` で明示的に従来の非対話出力へフォールバックする。

### 6.2 Preview Diagnostics

- Preview では一部の strict build failure を warning + next action に変換して PreviewSession に保存できる。
- PreviewSession には少なくとも次を保存する。
  - generated `capsule.toml`
  - inference attempt id
  - retry count
  - last smoke failure class / message / stderr excerpt
  - manual fix 適用済みかどうか
- Promotion / publish では PreviewSession の warning をそのまま success 扱いしてはならず、artifact hash と install object の整合性を再検証する。

### 6.3 Preview Storage

- preview metadata: `~/.ato/previews/<preview_id>/metadata.json`
- preview manifest snapshot: `~/.ato/previews/<preview_id>/capsule.toml`
- promoted install metadata: `<store>/<publisher>/<slug>/<version>/promotion.json`
- promoted runtime tree: `~/.ato/runtimes/promoted/<publisher>/<slug>/<version_hash>/`
- legacy installed runtime tree: `~/.ato/runtimes/<publisher>/<slug>/<version_hash>/`
- 上記の保存先は衝突してはならず、preview cleanup は promoted install object を削除してはならない。

## 7. IPC Broker 責務

ato-cli は **IPC Broker** として以下の責務を担う（CAPSULE_IPC_SPEC v1.1）:

| 責務                       | 説明                                                                          | Phase |
| -------------------------- | ----------------------------------------------------------------------------- | ----- |
| **Service 解決**           | `[ipc.imports]` の `from` を Local Registry → Local Store → Error で解決      | 1     |
| **ランタイムディスパッチ** | Service の `capsule.toml` を `route_manifest()` で Source/OCI/Wasm に振り分け | 1     |
| **IPC Registry**           | 起動中の Shared Service のメタ情報をプロセスメモリ上で管理                    | 1     |
| **RefCount**               | atomic な参照カウント管理。idle_timeout 後に Service 停止                     | 1     |
| **Token 管理**             | Bearer Token の発行・失効・通知 (constant-time 検証)                          | 1     |
| **Schema 検証**            | Service への転送前に JSON Schema 検証                                         | 1     |
| **DAG 統合**               | eager 依存を `_ipc_*` ノードとして DAG に追加、循環検出                       | 1     |
| **環境変数注入**           | `CAPSULE_IPC_*` を各ランタイムに注入 (−−env / WASI env / nacelle 経由)        | 1     |
| **Lazy 起動**              | 初回 `capsule/invoke` 時にオンデマンドで Service を起動                       | 1     |
| **MCP ブリッジ**           | Capability を MCP Tool として外部 AI エージェントに公開                       | 3     |

> **なぜ ato-cli なのか:** `router.rs` が Source/OCI/Wasm の 3 ランタイムにディスパッチするため、
> nacelle に Broker を置くと OCI/Wasm ランタイムの Capsule が IPC に参加できない。
> 「Smart Build, Dumb Runtime」原則に従い、IPC オーケストレーション（Smart）は ato-cli に、プロセス隔離（Dumb）は各 Engine に委譲する。

## 8. 実装状況

| 機能                                   | 状態      | ファイル                     |
| -------------------------------------- | --------- | ---------------------------- |
| ランタイムルーティング                 | ✅ 実装済 | `src/router.rs`              |
| Guest プロトコル (guest.v2)            | ✅ 実装済 | `src/commands/guest.rs`      |
| tsnet IPC トランスポート               | ✅ 実装済 | `core/src/tsnet/ipc.rs`      |
| IPC Broker (Registry, RefCount, Token) | ◐ 部品実装 + `ato run` launch context / `ato ipc {start,stop,status,invoke}` へ接続済み | `src/ipc/{broker,inject,registry,token}.rs`, `src/commands/{open,ipc}.rs`, `src/executors/launch_context.rs` |
| `ato ipc` サブコマンド                 | ✅ 実装済 (`status` / `start` / `stop` / `invoke`) | `src/commands/ipc.rs`        |
| Schema 検証                            | ✅ `ato build` / `ato run` / `ato validate` / `ato ipc invoke` へ接続済み | `src/ipc/schema.rs`, `src/ipc/validate.rs`, `src/commands/{build,open,ipc,validate}.rs` |
| DAG 統合                               | ❌ 未実装 | —                            |
| `ato validate` IPC チェック            | ✅ 実装済 | `src/ipc/validate.rs`, `src/commands/validate.rs` |
| `ato install`                          | ✅ 実装済 | `src/install.rs`             |
| PreviewSession / DerivedExecutionPlan  | ✅ 実装済 | `src/preview.rs`             |
| preview-aware validation / guard       | ✅ 実装済 | `core/src/types/manifest.rs`, `core/src/execution_plan/guard.rs`, `src/commands/{build,open}.rs` |
| promotion provenance (`promotion.json`) | ✅ 実装済 | `src/install.rs`             |
| promoted runtime namespace             | ✅ 実装済 | `src/runtime_tree.rs`        |
| `ato source register`                  | ✅ 実装済 | `src/source.rs`              |
| JSON-RPC 2.0 移行                      | ◐ `ato ipc invoke` request / error envelope は実装済み（broker 全面移行は未完） | `src/ipc/jsonrpc.rs`, `src/commands/ipc.rs` |

## 9. 依存

- capsule-core / nacelle (engine)
- clap / anyhow / serde
