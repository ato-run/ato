# Implementation TODO

**更新日:** 2026-04-25

> 進捗チェック用。完了済みは `[x]`、未着手は `[ ]`。

---

## 現在の優先タスク（2026-04-25）

### 🚀 V1.1: クロスプラットフォーム配布 (P1)

**RFC:** `docs/rfcs/draft/CROSS_PLATFORM_DISTRIBUTION_SPEC.md`

**方針:** GPUI + Wry のまま。App Bundle + symlink (B) + Homebrew Cask (G) で配布。

#### macOS

- [ ] `app.rs` に CLI symlink 自動作成ロジック追加 ("Install CLI" メニュー / 初回起動ダイアログ)
  - `/usr/local/bin/ato` → `Ato Desktop.app/Contents/Helpers/ato`
  - フォールバック: `~/.local/bin/ato`
- [ ] Homebrew Cask formula 作成 (`ato.rb`)
  - `brew install --cask ato` で GUI + CLI 同時インストール
- [ ] DMG パッケージング (xtask に `--dmg` オプション追加)
- [ ] コード署名 + Notarization (Apple Developer ID)

#### Windows

- [ ] GPUI DirectX 11 バックエンドでの ato-desktop ビルド確認
- [ ] MSI/NSIS インストーラー作成 (`ato-desktop.exe` + `ato.exe` + PATH 登録)
- [ ] `ato://` / `capsule://` URL スキーム登録 (レジストリ)

#### Linux

- [ ] GPUI Vulkan/X11 バックエンドでの ato-desktop ビルド確認
- [ ] AppImage パッケージング (desktop + CLI 同梱)
- [ ] Homebrew on Linux 対応 (`brew install --cask ato`)

### 🎬 Feature 2: Schema-Driven Dynamic Config UI (完了)

- [x] Day 1: ConfigField/ConfigKind schema + back-compat
- [x] Day 2: E103 missing_schema enrichment + preflight
- [x] Day 3: Desktop orchestrator JSON parse + PendingConfigRequest
- [x] Day 4: GPUI ConfigModal (Secret-only MVP) + re-arm loop
- [x] Day 5: Non-secret config persistence + direct env injection
- [x] Day 6: URL fix (/api/capsules -> /v1/capsules), mock eject, v0.3 normalizer, byok manifest upgrade
- [x] Day 6.5: SQL fixture script + registry E2E verification
- [x] Bug fix: session start authoritative_lock (ato.lock.json loading)

### 🏗️ V2.0+: モバイル対応 (P3 - 設計のみ)

**RFC:** `docs/rfcs/draft/CROSS_PLATFORM_DISTRIBUTION_SPEC.md` Section 4.3

- [ ] `HostShell` trait 設計・抽出 (DesktopShell から共通インターフェースを分離)
- [ ] capsule-core UniFFI バインディング生成
- [ ] iOS: SwiftUI shell PoC (Wry guest WebView + capsule-core FFI)
- [ ] Android: Jetpack Compose shell PoC (Wry guest WebView + capsule-core FFI)

---

## 過去の優先タスク

### 🧪 Ubuntu 実機インストールテスト (P0 completed 2026-04-23)

**目的**: `curl https://ato.run/install | sh` が実際の Ubuntu 環境（OCI インスタンス）で正常動作することを確認する。

**環境**: `ssh oci-linux-test`（Ubuntu 24.04 LTS / aarch64）

#### テストケース

| # | ケース | 確認コマンド | 期待結果 |
|---|--------|-------------|---------|
| TC-1 | インストールスクリプトが HTTP 200 で取得できる | `curl -sI https://ato.run/install \| head -3` | `HTTP/2 200`（`/install` → `/install.sh` へ 301 redirect） |
| TC-2 | インストールスクリプトが正常に実行される | `curl -fsSL https://ato.run/install \| sh` | エラーなしで完了。`~/.local/bin/ato` が生成される |
| TC-3 | `ato` が PATH に通っている | `which ato` または `~/.local/bin/ato --version` | バージョン文字列が表示される（例: `ato 0.4.73`） |
| TC-4 | 最新リリースバイナリが取得されている | `ato --version` の出力を `gh release view --repo ato-run/ato-cli` の latest と照合 | 一致する |
| TC-5 | `ato run` が基本動作する | `ato run npm:cowsay hello` | cowsay の出力が表示される |
| TC-6 | 冪等性：2 回目の実行でエラーにならない | 再度 `curl -fsSL https://ato.run/install \| sh` を実行 | 上書きインストールまたはスキップメッセージ。エラーなし |
| TC-7 | sudo なし・非 root で実行できる | 一般ユーザーで TC-2 を実行 | `~/.local/bin/` 以下へのインストールが完了する（system-wide 不要） |
| TC-8 | `~/.local/bin` が既存 PATH と競合しない | `echo $PATH` で確認。必要なら shell reload | 既存コマンドが上書きされていない |

#### 実行手順

```bash
ssh oci-linux-test

# クリーン環境確認
which ato 2>/dev/null && echo "ato already installed" || echo "fresh install"

# TC-1: スクリプト取得確認
curl -sI https://ato.run/install | head -5

# TC-2〜4: インストールと動作確認
curl -fsSL https://ato.run/install | sh
source ~/.bashrc 2>/dev/null || source ~/.profile 2>/dev/null
ato --version

# TC-5: 基本動作
ato run npm:cowsay hello

# TC-6: 冪等性
curl -fsSL https://ato.run/install | sh

# TC-7: 非 root 確認（一般ユーザーで実行済みであれば OK）
ls -la ~/.local/bin/ato
```

#### チェックリスト

- [x] TC-1: インストールスクリプトが `HTTP 200` で返る（`_redirects` に `/install → /install.sh` を追加済み）
- [x] TC-2: `curl ... | sh` がエラーなしで完了する
- [x] TC-3: `ato --version` が動作する
- [x] TC-4: バージョンが最新リリース（v0.4.73 以降）と一致する
- [x] TC-5: `ato run npm:cowsay hello` が動作する（`prepare` を lifecycle script チェックから除外済み）
- [x] TC-6: 2 回目のインストールがエラーなし
- [x] TC-7: 非 root でインストール完了
- [x] TC-8: 既存コマンドとの PATH 競合なし

---

## 現在の優先タスク（2026-04-16）

### 📦 samples/ reorganization (P0 shipped 2026-04-20)

`samples/` is now an independent git repository with the four-tier layout
(00-quickstart / 01-capabilities / 02-apps / 03-limitations + playground),
CI foundation (`health.toml` schema + matrix builder + per-layer runner),
and the v0.5+ backwards-compatibility lint (`lint-min-ato-version`).

Plan and research artifacts:
`claudedocs/research_orchestrated_samples_onboarding_20260420/PLAN.md`

- [x] P0: bootstrap repo + tier skeleton + CI foundation
- [x] P0: delete 4 stubs (iphone-3d, react-dnd-kanban, shadcn-admin, openclaw-agent)
- [x] P0: move test fixtures to `tests/` and dev mocks to `dev-internal/`
- [ ] P1: author flagship samples (`byok-chat-openrouter`, `ollama-local-rag`, `scraper-with-allowlist`)
- [ ] P1: migrate 12 keeper samples from `samples/` root into tier directories
- [ ] P1: implement L2 functional assertions in `run-sample-checks.mjs` (stdout/http/file matchers)
- [ ] P1: implement L5 regression / L6 perf budgets
- [ ] P1: wire `.github/actions/setup-ato/` to install `stable` / `main` / pinned
- [ ] P1: sticky PR status comment + `catalog` job that regenerates README matrix
- [ ] P1: push `samples/` to `github.com/ato-run/ato-samples`
- [ ] P2: author the 5 Tier-03 limitation demos
- [ ] P2: formalize 11 nested repos (`hello-capsule`, `ato-onboarding`, 9 under `playground/byok-suite/*`) as submodules once their working trees are clean
- [ ] P3: CONTRIBUTING policy for community PRs, flaky-sample quarantine workflow

### 🌐 English-first migration (decided 2026-04-20)

Public-facing surfaces go EN-only; JA becomes a translation layer, not the
source. Driven by the `ato-samples` policy (EN-only READMEs/comments) — the
same standard applies to the rest of the workspace.

- [ ] `apps/ato-cli/`: make `README.md` the canonical EN version, archive `README_JA.md`; audit help strings, error messages, inline comments; migrate JA → EN
- [ ] `apps/ato-desktop/`: audit UI strings, settings labels, menu items, tooltips, help text; migrate JA → EN. Leave i18n scaffolding for after the EN baseline lands (don't mix translations with the migration)
- [ ] `apps/desky/`: same audit; prioritize user-visible surfaces over code comments
- [ ] `apps/ato-docs/`: confirm EN is the source of truth; treat JA as a future translation layer decoupled from EN revisions
- [ ] `apps/ato-store-web/` + `apps/ato-play-web/`: audit for user-visible JA strings
- [ ] Cross-repo: add a lint that rejects new JA characters in user-visible strings unless routed through an i18n helper
- [ ] Update contributor docs (`AGENTS.md`, `CLAUDE.md`) to state EN-only for new code

### 🚧 Phase 13: Capsule IPC

現在のアクティブ作業は **Phase 13** (Capsule IPC) です。詳細は下の Phase 13 セクションを参照。

**残タスク（主要）:**

- [x] **13a.1〜13a.4**: nacelle — IPC ソケットパスの Sandbox 許可、`ipc_env` 透過、readiness 報告 (完了 2026-04-29、ADR-007)
- [ ] **13b.9**: Guest プロトコル JSON-RPC 2.0 移行 (`GuestAction` → `capsule/invoke`)
- [ ] **Phase 8.4**: Desktop での Profile 表示・キャッシュ
- [ ] **Phase 9.2〜9.4**: license.sync — entitlements 注入、Desktop 統合
- [ ] **Phase 10.2**: sync-fs マルチマウント対応 (WebDAV 統合)
- [ ] **Phase 13c**: ato-desktop — Guest Mode IPC UI

**完了直近（ato-cli）:**

- [x] ato-cli `~/.ato` 完全移行（`~/.capsule` → `~/.ato`）
- [x] Bridge Auth API 雛形 (`/v1/auth/bridge/*`)
- [x] IPC モジュール基盤 (13b.1〜13b.8, 13b.10, 13b.11)

---

## Personal Dock First RFC（残実装）

**更新日:** 2026-03-04

### ✅ 2026-03-04 実施ログ（staging remote）

- `npx wrangler d1 migrations apply DB --env staging --remote` で `0025_capsules_scoped_slug.sql` / `0026_official_submission_requests.sql` 適用完了
- `PRAGMA index_list('capsules')` で `idx_capsules_publisher_slug`（unique=1）を確認
- `SELECT publisher_id, slug, COUNT(*) ... HAVING c > 1` は結果0件（重複なし）
- 残タスクは「認証付き POST 本番同等テスト（submit/items ingest）」

### 🚧 2026-03-04 実装開始: Bridge Auth PR1 + `.ato` 完全移行（非互換）

- [x] `ato-cli` の主要実行パスを `~/.ato` へ切替（config/store/run/logs/runtimes/keys/cas/toolchains）
- [x] `ato-store` に Bridge Auth API 雛形を追加（`/v1/auth/bridge/init|poll|exchange|cancel|authorize`）
- [x] D1 migration を追加（`auth_sessions`, `auth_audit_logs`）
- [ ] `ato-cli` login フローを Bridge Auth（PKCE S256 + poll/exchange）へ接続
- [x] 秘密ストア失敗時の fail-closed（平文保存禁止）を実装（`ATO_TOKEN` 優先 + keyring保存、平文フォールバックなし）
- [ ] `.capsule` 参照の残存箇所をゼロ化（ドキュメント/テスト含む）

### 🔴 P0: 仕様・契約の固定

- [x] `docs/specs/STORE_SPEC.md` に `/v1/docks` 契約を反映（`POST /v1/docks`, `GET /v1/docks/:handle`, `GET /v1/docks/:handle/items`, `POST /v1/docks/:handle/items`, `POST /v1/docks/:handle/submit`）
- [x] `docs/specs/STORE_WEB_SPEC.md` に Dock導線を反映（`/publish`, `/d/:handle`, Three Gates, Premium Path）
- [x] RFC本文と実装差分の同期（precheck 202受理・submit/items の段階導入ステータス明記）

### 🟠 P1: API 実装の仕上げ

- [x] `POST /v1/docks/:handle/items` をフル取り込み対応（既存 private publish 契約へ接続、artifact保存・release確定まで）
- [x] `POST /v1/docks/:handle/submit` を 501 から実処理へ移行
- [x] D1 マイグレーション追加（`official_submission_requests` テーブル + index）
- [x] 申請状態取得APIを追加（Queue / Changes Requested / Verified）

### 🟡 P1: Web 実装の仕上げ

- [x] Official Submission の Checklist UI を API連動化（現在は予約導線のみ）
- [x] D&D Gate を presign アップロードまで接続（`/v1/uploads/presign` + 進捗表示 + 失敗時リトライ）
- [x] `/d/:handle` を trust/バージョン/申請状態表示まで拡張
- [x] `Personal Dock (Unverified)` バッジを実データ連動化（固定表示から移行）

### 🟢 P2: CLI / ドキュメント整備

- [x] `apps/ato-cli/README.md` に Dock-first フローを追記（既存コマンドのみ）
- [x] `apps/ato-cli/README_JA.md` に Dock導線を追記
- [x] CLI help 文言を Dock基準へ更新（新サブコマンド追加なし）

### 🔵 P2: テスト・検証

- [x] `apps/ato-store/src/tests/docks-routes.test.ts` に submit実処理ケースを追加
- [x] `apps/ato-store-web` 側の Three Gates UI テスト追加（CLIコピー / GitHub導線 / D&D precheck）
- [x] Dock E2E（`/publish` → Dock作成 → `/d/:handle` 表示 → submit）を追加
- [x] 変更範囲に対する再検証コマンドを固定（対象テスト + lint）

固定再検証コマンド:

```bash
pnpm -C apps/ato-store exec vitest run src/tests/docks-routes.test.ts
pnpm -C apps/ato-store exec tsc --noEmit
pnpm -C apps/ato-store-web exec astro check
```

### 🟣 P3: ルーティング最終化

- [x] `handle.ato.run` 導入計画を実装へ分解（Worker host rewrite / staging検証 / fallback運用）
- [x] `/d/:handle` と `handle.ato.run` の正本戦略を確定して運用手順化

---

## Phase 0-7: 基盤実装 ✅

<details>
<summary>Phase 0-7 詳細（完了済み）</summary>

### Phase 0: Baseline 固定化

- [x] `.capsule` v2 仕様の確定（PAX TAR + JCS署名）
- [x] Sidecar (tsnet) + SOCKS5 Allowlist
- [x] Supervisor Mode / multi-service
- [x] JIT Provisioning (Python/Node)

### Phase 1: `.sync` Runtime

- [x] VFSで`payload`をゼロコピー提示
- [x] `sync.wasm` Strict Sandbox 実装
- [x] `payload`アトミック差し替え
- [x] nacelle への統合
- [x] TTL管理機能
- [x] LAN/WANの共有ポリシー切替

### Phase 2: App-in-App (Guest Mode)

- [x] Guest Mode仕様策定（Widget/Headless）
- [x] Host→Guestの最小権限委譲
- [x] Consumer/Owner Context分離
- [x] IPC/API定義

### Phase 3: P2P Discovery (MagNet)

- [x] mDNS Discovery PoC
- [x] DHT (Kademlia) Discovery PoC
- [x] GossipSub Sync PoC
- [x] Relay fallback (Circuit Relay/DCUtR)

### Phase 4: Schema Registry

- [x] Schema Registry 仕様策定
- [x] SchemaHash（JCS正規化）算出ロジック
- [x] `implements`/`SchemaHash` の解決ロジック
- [x] `mag://` Resolver 実装

### Phase 5: Security Hardening

- [x] `capsule.lock` の Integrity Check 実装
- [x] 鍵ローテーション/失効リスト
- [x] Trust UX (TOFU/Petnames)

### Phase 6: SDK & DX

- [x] `.sync` 生成SDK
- [x] Guest Mode API
- [x] BlockSuite PoC (Single Player)

### Phase 7: Desktop App Gaps

- [x] Install FlowのTauriコマンド実装
- [x] セキュリティ操作のTauriコマンド実装
- [x] 監査ログのTauriコマンド実装
- [x] Trusted Signers管理UI

</details>

---

## Phase 8: Identity Layer 🚧

**仕様:** [IDENTITY_SPEC.md](docs/specs/IDENTITY_SPEC.md)

### 8.1 capsule-core `did:key` サポート

- [x] `identity.rs` モジュール作成
- [x] `to_did_key()` / `from_did_key()` 変換関数
- [x] `public_key_to_did()` / `did_to_public_key()` 関数
- [x] `multibase`, `unsigned-varint` 依存追加（Base58は自前実装で対応）
- [x] 既存 `StoredKey` との統合
- [x] ユニットテスト (7 tests passing)

### 8.2 ato-desktop Keychain 統合

- [x] `keyring` クレート導入
- [x] `IdentityManager` 構造体実装
- [x] Tauri Command: `identity_get`, `identity_generate`, `identity_sign`, `identity_exists`, `identity_delete`, `identity_reset`
- [x] Frontend hooks (`useIdentity`)
- [x] Settings 画面: 「My DID」表示

### 8.3 署名フロー統合

- [x] `ato sign` コマンドの `did:key` 対応
- [x] `parse_developer_key()` の `did:key` サポート
- [x] `StoredKey.did()` メソッド追加
- [x] JSON署名形式サポート（StoredKeyファイル読み込み）
- [x] 署名検証 CLI コマンド (`ato verify`)
- [x] `[signature]` セクションのTOMLシリアライズ
- [x] `sync-format` への検証ロジック追加

### 8.4 Profile Capsule

- [x] `profile.sync` スキーマ定義 (JSON Schema)
- [x] `ProfileManifest` 型定義 (capsule-core)
- [x] `ato profile create` コマンド
- [x] `ato profile show` コマンド
- [ ] Desktop での Profile 表示
- [ ] Profile キャッシュ

---

## Phase 9: License Protocol 📅

**仕様:** [LICENSE_SPEC.md](docs/specs/LICENSE_SPEC.md)  
**依存:** Phase 8 完了後

### 9.1 license.sync フォーマット

- [x] manifest.toml スキーマ定義 (JSON Schema)
- [x] `capsule-core` に License 型追加
- [x] `LicenseVerificationResult` 型定義
- [ ] license.json パーサー
- [ ] sync.wasm テンプレート (renewal)

### 9.2 nacelle 検証ロジック

- [x] `verify_license()` 実装
- [x] グレース期間ロジック (7日)
- [ ] entitlements 環境変数注入 (`CAPSULE_ENTITLEMENTS`)
- [ ] 検証結果のキャッシュ

### 9.3 Desktop 統合

- [ ] `~/.capsule/licenses/` ディレクトリ管理
- [ ] `index.json` 管理
- [ ] Tauri Commands: `license_list`, `license_get`, `license_import`
- [ ] Settings > Licenses UI

### 9.4 Registry 連携 (Stub)

- [ ] ライセンス発行 API モック
- [ ] Desktop からのライセンス取得フロー

---

## Phase 10: Dynamic Binding 📅

**仕様:** [ASSOCIATION_SPEC.md](docs/specs/ASSOCIATION_SPEC.md)  
**依存:** Phase 8 完了後（Phase 9 と並行可）

### 10.1 Association Registry

- [x] `associations.json` スキーマ定義 (JSON Schema)
- [x] Registry 管理モジュール (`associations.rs`)
- [x] MIME Type マッチングロジック (ワイルドカード対応)
- [x] Tauri Commands 実装
- [x] Frontend hooks (`useAssociations`)

### 10.2 Dual-Mount System

- [x] `CapsuleContext` 型定義
- [x] `DualMountManager` 実装
- [x] ポート管理・衝突回避
- [x] Tauri Commands 実装
- [x] Frontend hooks (`useDualMount`)
- [ ] `sync-fs` のマルチマウント対応 (WebDAV サーバー統合)
- [ ] マウントポイント URL 生成
- [ ] アンマウント処理

### 10.3 コンテキスト注入

- [x] 環境変数注入 (`CAPSULE_DATA_URL` 等)
- [x] `window.capsule` オブジェクト注入 (JS 生成)
- [ ] postMessage ハンドラー

### 10.4 UI

- [ ] アプリ選択ダイアログ
- [ ] Settings > Default Apps 画面
- [ ] 「常にこのアプリで開く」チェックボックス

### 10.5 アプリカプセル

- [x] `app.sync` スキーマ定義
- [x] `app.supports` による自動登録
- [x] サンプルアプリ (Hello Capsule)

---

## Phase 11: Protocol Registry 📅

**依存:** Phase 9, 10 完了後

### 11.1 Registry API

- [x] API 仕様策定 (OpenAPI)
- [ ] ato-coordinator エンドポイント実装
- [ ] 認証 (SPIFFE / API Key)

### 11.2 ato install

- [x] `ato install <app-id>` コマンド
- [x] Registry からの .sync ダウンロード
- [x] 自動 Association 登録
- [ ] 署名検証 (verify 連携)

### 11.3 決済連携

- [x] Stripe Checkout 統合 (Go client/handler)
- [x] Webhook → ライセンス発行 (license service)
- [x] Desktop へのライセンス配信 (license_receive command)

### 11.4 分散 Registry

- [x] DNS TXT レコード解決
- [x] JSON Registry (well-known endpoint)
- [x] `ato registry` サブコマンド
- [ ] 将来: DHT/Git ベース

---

## Phase 12: Network Integration 📅

**依存:** Phase 10 完了後

### 12.1 tsnetd 完全統合

- [x] Desktop ↔ tsnetd IPC 安定化
- [x] 自動起動/再接続
- [x] ステータス表示 UI (useTailnet hook)
- [ ] Settings UI 統合

### 12.2 Remote Mount

- [x] P2P 経由 WebDAV マウント (SOCKS5 proxy)
- [x] キャッシュ戦略 (CacheConfig)
- [ ] オフラインフォールバック
- [ ] Desktop UI 統合

### 12.3 App オンデマンド取得

- [x] Registry/P2P からの .sync 取得 (AppFetcher)
- [x] 署名検証 (digest check)
- [x] ローカルキャッシュ
- [x] `context.json` 競合ポリシー
- [x] Trust UX のユーザー体験

---

## Test Coverage TODO（不足分）

### capsule-sync（unit） → sync-format に移行済み

- [x] `.sync` フォーマット異常系: `payload` 非Stored圧縮の拒否
- [x] `.sync` フォーマット異常系: `manifest.toml` 欠落
- [x] `.sync` フォーマット異常系: `sync.wasm` 欠落
- [x] `context.json` / `sync.proof` の存在有無による分岐
- [x] `SyncArchive::update_payload()` の再オープン整合性（entry/offset更新）

### nacelle（integration）

- [ ] `SyncRuntime::execute_and_update()` が payload を更新すること
- [ ] `auto_update_if_expired()` が TTL 期限時のみ動作すること
- [ ] `execute_wasm()` の strict sandbox: FS/Network allowlist逸脱拒否
- [ ] `sync.wasm` 未同梱時のエラー

### ato-cli（e2e）

- [x] guest: `ReadContext` / `WriteContext` の許可/拒否 (sync-rs 移行完了)
- [x] guest: `ExecuteWasm` の許可/拒否（Owner/Consumer差）(sync-rs 移行完了)
- [x] guest: `write_allowed=false` の push 拒否 (sync-rs 移行完了)

### tsnet sidecar（e2e）

- [ ] `allow_net` の deny/allow 挙動（CIDR / wildcard domain）
- [x] `StartServe/StopServe/ServeStatus` の正常系
- [x] `StartServe` の不正 target_addr（loopback以外）拒否

### ato-desktop sync（unit/integration）

- [x] `SyncStorage` の mounts.json 永続化（再起動復元）
- [ ] `SyncManager::list_files()` が消失ファイルを prune すること
- [ ] LRUキャッシュの eviction（DEFAULT_CACHE_CAPACITY）
- [ ] `sync-update` イベント emit とUI反映（hook）
- [ ] Share server: local/tailnet address の出力
- [ ] `import_from_url` が SOCKS5 (tsnet) を使用できること

### ato-desktop UI（Playwright）

- [ ] Sync画面: `.sync` のマウント/アンマウント
- [ ] Sync画面: 手動Syncボタンで更新表示
- [ ] Sync画面: Share Start/Stop と Tailnet 주소表示
- [ ] Sync画面: Import URL で追加されること

---

## Phase 10-12 Test Coverage（今回追加分）

### Phase 10: Association Registry (11 tests)

- [x] `match_content_type` exact/wildcard tests
- [x] `resolve` priority ordering
- [x] `resolve` exact match before wildcard
- [x] `set_default` clears other defaults
- [x] `register` replaces existing entries
- [x] `unregister` removes entries
- [x] `get_default` fallback to highest priority
- [x] `AppSource` serialization (lowercase)

### Phase 11: Registry/Install (9 tests)

- [x] DNS TXT record parsing (valid/invalid/no-key)
- [x] Registry cache put/get/clear
- [x] Cache domain encoding (dots to underscores)
- [x] RegistryResolver default/with_fallback
- [x] DiscoverySource serialization

### Phase 12: Network Integration (15+ tests)

- [x] RemoteMountConfig defaults
- [x] CacheConfig defaults
- [x] parse_addr validation (host:port)
- [x] FetchConfig defaults
- [x] FetchSource serialization
- [x] AppFetcher creation and cache operations
- [x] useTailnet hook: getTailnetStateInfo all states

## Phase 13: Capsule IPC 🚧

### 13a: nacelle — Sandbox Enforcer 対応 ✅ (完了 2026-04-29)

> nacelle は IPC の「内容」には関与せず、ato-cli が注入する IPC Transport パスの
> Sandbox 許可と環境変数の透過のみを担当する (Smart Build, Dumb Runtime)。
>
> **設計決定**: `claudedocs/research_phase13a_sandbox_best_practices_20260429.md`
> および `docs/rfcs/accepted/ADR-007-macos-sandbox-api-strategy.md` を参照。
> macOS は `sandbox_init(flags=0)` 経由の動的 SBPL を採用 (nono 参照実装)。

#### 13a.1 IPC Transport の Sandbox 許可

- [x] `system/sandbox/mod.rs`: ato-cli からの IPC ソケットパスを受け取るインターフェース追加
  - `SandboxPolicy.ipc_socket_paths: Vec<PathBuf>` + `with_ipc_socket_paths()` builder
  - JSON stdin の `ipc_socket_paths: Vec<String>` フィールド対応
- [x] `system/sandbox/macos.rs` (macOS): Seatbelt プロファイルに IPC ソケットパスを動的追加
  - `generate_sbpl_profile()` を `#[allow(dead_code)]` 解除して活性化
  - `apply_seatbelt_sandbox()` を `sandbox_init(flags=0)` の動的 SBPL 経路に切替
  - file-read*/file-write* + network* (remote/local unix-socket) の両ルールを emit
  - Mach IPC keychain deny (`com.apple.secd` 等) を追加 (nono パターン)
- [x] `system/sandbox/linux.rs` (Linux): Landlock ルールセットに IPC ソケットパスを動的追加
  - `path_beneath_rules(path, AccessFs::from_all)` で IPC パス + 親ディレクトリ fallback
- [x] テスト: IPC パスが SBPL に含まれること、Mach IPC keychain deny、symlink 解決
  (`crates/nacelle/src/system/sandbox/macos.rs::tests` 9 件)

#### 13a.2 環境変数の透過

- [x] `nacelle internal exec` の JSON 入力に `ipc_env: Vec<(String, String)>` を追加
  (`crates/nacelle/src/cli/commands/internal.rs:180` `ExecEnvelope`)
- [x] 子プロセス spawn 時に `CAPSULE_IPC_*` 環境変数を透過 (`merge_workload_env()`)
- [x] nacelle 自身は値を解釈しない (Dumb Runtime — broker は ato-cli 側で完結)
- [x] テスト: ato-cli `ipc_socket_e2e` が `CAPSULE_IPC_*` を子プロセスで受け取って Unix socket
  に書き込む経路を確認

#### 13a.3 Readiness Probe の ato-cli への報告

- [x] Supervisor Mode の readiness_probe 結果を JSON stdout で報告 (`NacelleEvent::IpcReady`)
  - 実装フォーマットは `{"event":"ipc_ready", "service":"...", "endpoint":"...", "port":N}`
    (TODO 草案の `"type"` ではなく `"event"` を採用 — `#[serde(tag = "event")]` で統一)
- [x] `run_supervisor_mode()` に報告ロジック (`crates/nacelle/src/manager/r3_supervisor.rs:188`)
- [x] テスト: 3 件 (`test_nacelle_event_ipc_ready_serialization` /
  `test_nacelle_event_ipc_ready_with_port` / `test_nacelle_event_ipc_ready_wire_contract`)

#### 13a.4 Engine Interface 拡張

- [x] `nacelle internal exec` の入力 JSON スキーマに IPC フィールド追加
  (`spec_version` v1.0/v2.0 両方で `ipc_env` / `ipc_socket_paths` 受理)
- [x] `nacelle internal features` の応答に `"ipc_sandbox": true` 追加
  (`Capabilities` struct + fail-closed: backend が無ければ false)
- [x] ENGINE_INTERFACE_CONTRACT.md 更新 (§5「Sandbox semantics」節を追加)

#### 13a の今後の宿題（Phase 13a スコープ外）

- [ ] Landlock ABI probe (V6→V1, nono パターン) — 中期 RFC
- [ ] Landlock IPC scoping (`LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET`) — V5+ 利用時
- [ ] macOS `sandbox_init` 後継 API 監視 (ADR-007 §4 — 6 ヶ月毎レビュー)

---

### 13b: ato-cli — IPC Broker Core (見積 3週間)

> ato-cli が全ランタイム (Source/OCI/Wasm) 横断で IPC を統括する。
> 既存の `router.rs` (343行) と `guest.rs` (762行) を拡張する形で実装。

#### 13b.1 IPC モジュール基盤

- [x] `src/ipc/mod.rs` モジュール作成 (registry, token, schema, transport, broker)
- [x] `src/ipc/types.rs`: IPC 共通型定義
  - `IpcServiceInfo { name, pid, endpoint, transport, capabilities, refcount, started_at, runtime_kind }`
  - `IpcToken { value, ttl, scoped_capabilities, created_at }`
  - `IpcEndpoint` enum (Stdio, UnixSocket, Tcp, Tsnet)
- [x] `src/ipc/registry.rs`: IPC Registry 実装
  - `IpcRegistry` struct (in-memory HashMap)
  - `register()`, `unregister()`, `lookup()`, `list()` メソッド
  - スレッドセーフ (`Arc<Mutex<...>>`)

#### 13b.2 参照カウント (RefCount)

- [x] `src/ipc/refcount.rs`: 参照カウント管理
  - `SharedService { ref_count: AtomicU32, idle_timer, state }` struct
  - `acquire()` — refcount++, cancel idle timer
  - `release()` — refcount--, refcount==0 → start idle timer
  - ロック順序強制: `idle_timer` → `state` (逆順禁止)
- [x] Sharing Mode 実装: singleton / exclusive / daemon
  - `exclusive`: Client 終了 = 即 SIGTERM
  - `daemon`: idle_timeout 無視
- [x] テスト: refcount の増減、idle timer の発火/キャンセル

#### 13b.3 Token 管理

- [x] `src/ipc/token.rs`: Bearer Token の発行・失効
  - `generate_token(capabilities, ttl)` → `IpcToken`
  - `validate_token(token)` — constant-time comparison 必須
  - `revoke_token(token)` + `capsule/internal.tokenRevoked` Notification 送信
- [x] TTL 管理: デフォルト 24h、`[ipc.exports.sharing]` でカスタマイズ可
- [x] テスト: Token 生成、検証、失効、TTL 期限切れ

#### 13b.4 Schema 検証

- [x] `src/ipc/schema.rs`: JSON Schema Validator
  - `validate_input(schema_path, input)` → `Result<(), SchemaError>`
  - `SchemaError` に `hint` フィールド含む (開発者向けヒント)
- [x] `max_message_size` (1MB) の強制
- [x] `jsonschema` crate 依存追加
- [x] テスト: 正常入力、不正入力、サイズ超過

#### 13b.5 DAG 統合

- [x] `capsule.toml` パーサーに `[ipc.exports]` / `[ipc.imports]` セクション追加
  - `capsule-core` の manifest 型に `IpcExports` / `IpcImports` 追加
- [x] `src/ipc/dag.rs`: IPC 依存の DAG 統合
  - `build_ipc_dag(imports, existing_dag)` → eager 依存を `_ipc_*` ノードとして追加
  - 予約プレフィックス (`_ipc_`, `_setup`, `_main`) 衝突検出
  - 循環依存検出
- [x] テスト: DAG ノード追加、循環検出、予約名衝突

#### 13b.6 Service 解決・起動

- [x] `src/ipc/broker.rs`: `resolve()` — `from` の解決
  - 解決順序: Local Registry (running) → Local Store (`~/.capsule/store/`) → Error
  - `"<name>"` / `"@<scope>/<name>:<semver>"` / `"./<path>"` の 3 形式対応
  - 自動ダウンロード禁止。`ato install <name>` を促すエラー
- [x] `src/ipc/broker.rs`: メインオーケストレーター構造
  - Service の `capsule.toml` を `route_manifest()` でランタイム決定
  - `executors/source.rs` / `oci.rs` / `wasm.rs` に振り分け
  - `activation = "eager"`: Client 起動前に Service 起動 + readiness wait
  - `activation = "lazy"`: 初回 `capsule/invoke` 時にオンデマンド起動
- [x] テスト: 解決成功、未インストールエラー、ランタイム振り分け

#### 13b.7 環境変数注入

- [x] `src/ipc/inject.rs`: `IpcContext` — manifest [ipc] パース → broker 解決 → env 生成
- [x] `executors/source.rs`: `run_bundle()` に `ipc_env` パラメータ追加、`cmd.env()` で注入
- [x] `executors/oci.rs`: `execute()` に `ipc_env` パラメータ追加、`--env` フラグで注入
- [x] `executors/wasm.rs`: `execute()` に `ipc_env` パラメータ追加、`cmd.env()` で注入
- [x] `commands/open.rs`: `IpcContext::from_manifest()` → 全 executor に ipc_env 渡し
- [x] 共通: `CAPSULE_IPC_<SERVICE>_URL/_TOKEN/_SOCKET` + `CAPSULE_IPC_PROTOCOL/TRANSPORT` 生成
- [x] テスト: IpcContext 生成（空/optional/required エラー/protocol markers）6件

#### 13b.8 JSON-RPC 2.0 Wire Protocol

- [x] `src/ipc/jsonrpc.rs`: JSON-RPC 2.0 パーサー/シリアライザー
  - `JsonRpcRequest` / `JsonRpcResponse` / `JsonRpcError` / `JsonRpcNotification` 型
  - Error Codes: -32700 – -32004 (CAPSULE_IPC_SPEC §8.2)
  - `data.hint` フィールド必須
- [x] `capsule/initialize` ハンドシェイク型定義
- [x] `capsule/invoke` 型定義
- [x] `capsule/ping` 型定義
- [x] テスト: ハンドシェイク、invoke、エラー応答、Notification

#### 13b.9 Guest Protocol JSON-RPC 2.0 移行 ✅ (完了 2026-04-29)

> **設計**: `claudedocs/plan_phase13b9_guest_jsonrpc_migration_20260429.md` (v2.1)
>
> **WASM/OCI 関連は defer**: `capsule/wasm.execute` メソッドおよび OCI/WASM
> executor の env 注入は本 PR スコープ外 (ランタイム整備待ち)。`ExecuteWasm`
> は guest.v1 envelope 経由でのみ引き続き利用可。

- [x] `src/cli/commands/guest.rs` に envelope 自動判別ロジックを追加
  (`jsonrpc=2.0` / `version=guest.v1` / 不明) — LSP 流の auto-detect
- [x] `src/cli/commands/guest_jsonrpc.rs` を新規作成 — JSON-RPC 2.0 wire layer
  - `handle_jsonrpc_request`, `method_to_action`, `parse_method_params`,
    `jsonrpc_error_from_guest`, `wrap_result`
  - 5 method を実装: `capsule/payload.{read,write,update}`, `capsule/context.{read,write}`
  - `capsule/wasm.execute` は `-32601 Method not found` (将来用に予約)
- [x] `src/cli/commands/guest.rs` に `dispatch_guest_action` 純粋関数を抽出
  (legacy/JSON-RPC 両 wire layer から共有)
- [x] `ensure_permissions` signature を `(action, role, &permissions)` に縮退
- [x] `src/adapters/ipc/guest_protocol.rs` に `GuestErrorCode::to_jsonrpc_code()` impl 追加
- [x] Host-Guest 環境変数を新命名規則に統一 (旧名は破棄、fallback なし):
  - `CAPSULE_IPC_PROTOCOL` (旧 `CAPSULE_GUEST_PROTOCOL`)
  - `CAPSULE_IPC_MODE` (旧 `GUEST_MODE`)
  - `CAPSULE_IPC_ROLE` (旧 `GUEST_ROLE`)
  - `CAPSULE_IPC_SYNC_PATH` (旧 `SYNC_PATH` ※ WASI mount は別レイヤー)
  - `CAPSULE_IPC_WIDGET_BOUNDS` (旧 `GUEST_WIDGET_BOUNDS`)
- [x] result/params に object wrapper 採用:
  - `payload.read` → `{ payload_b64: string }`
  - `context.read` → `{ value: any }`
  - write 系は `result: null`
- [x] テスト: 14 件 E2E (`tests/guest_jsonrpc_e2e.rs`) + 10 件ユニット (guest_jsonrpc::tests)
- [x] 既存 `guest_e2e.rs` (7 件) は無変更で全通過 — 後方互換確認

**ato-cli テスト合計**: 1312 passed / 0 failed

**残: WASM/OCI ランタイム整備後に実施**:
- [ ] `capsule/wasm.execute` method の正式公開
- [ ] OCI executor の `CAPSULE_IPC_*` env 注入 (`adapters/runtime/executors/oci.rs`)
- [ ] WASM executor の `CAPSULE_IPC_*` env 注入 (`adapters/runtime/executors/wasm.rs`)

#### 13b.10 `ato ipc` サブコマンド

- [x] `src/commands/ipc.rs` モジュール作成
- [x] `src/commands/mod.rs` に `pub mod ipc;` 追加
- [x] clap `SubCommand::Ipc` を `Cli` enum に追加
- [x] `ato ipc status` 実装
  - 出力: SERVICE / MODE / REFCOUNT / TRANSPORT / ENDPOINT / RUNTIME / UPTIME
  - `--json` フラグ
- [x] `ato ipc start` 実装 (capsule.toml の [ipc] パース + registry 登録)
- [x] `ato ipc stop` 実装 (SIGTERM/SIGKILL + registry 解除 + socket 削除)
- [x] テスト: status 出力のフォーマット検証

#### 13b.11 `ato validate` IPC チェック

- [x] `src/ipc/validate.rs` モジュール作成
- [x] `[ipc.exports.sharing]` と `[lifecycle]` ショートカットの併用をエラー (IPC-001)
- [x] `_ipc_` / `_setup` / `_main` 予約プレフィックス衝突検出 (IPC-002)
- [x] IPC 循環依存検出 (IPC-003)
- [x] `remote = true` Capability の警告表示 (IPC-005)
- [x] `from` の解決可能性チェック (Local Store 存在確認) (IPC-006)
- [x] 空 exports 警告 (IPC-007)
- [x] テスト: 各検証ルールの正常/異常系 (14テスト)

---

### 13c: ato-desktop — Guest Mode IPC UI (見積 2週間)

> Guest Capsule の JSON-RPC 2.0 Host Bridge と User Consent ダイアログを実装。
> 既存の `HostBridgeFrame.tsx` (344行) と `OwnerConsent` 型を拡張する形で実装。

#### 13c.1 Host Bridge JSON-RPC 2.0 移行

- [x] `src/lib/ipc-jsonrpc.ts`: JSON-RPC 2.0 型定義・パーサー・ヘルパー
  - `JsonRpcRequest` / `JsonRpcResponse` / `JsonRpcError` / `JsonRpcNotification`
  - Error Codes: -32700 – -32004 (CAPSULE_IPC_SPEC §8.2)
  - `IPC_METHODS` 定数: capsule/\* メソッド名一覧
  - `mapLegacyToJsonRpc()`: ATO*FS*_ → capsule/_ 変換
- [x] `HostBridgeFrame.tsx`: postMessage フォーマットを JSON-RPC 2.0 に移行
  - `ATO_FS_READ_SYNC` → `capsule/payload.read`
  - `ATO_FS_WRITE_SYNC` → `capsule/payload.write`
  - `ATO_FS_LIST_SYNC` → `capsule/payload.list`
- [x] JSON-RPC 2.0 ディスパッチャー (`dispatchJsonRpc()`)
- [x] 新規 method 追加:
  - `capsule/initialize` (ハンドシェイク + capabilities一覧返却)
  - `capsule/invoke` (スタブ — 13dで完全実装)
  - `capsule/ui.resize`, `capsule/ui.modeChange`
  - `capsule/lifecycle.shutdown` / `ready_to_shutdown`
  - `capsule/ping`
- [x] 後方互換: `ATO_FS_*` 形式も引き続きサポート (legacy path)
- [x] iframe sandbox 属性適用 (`allow-scripts allow-same-origin`)
- [ ] テスト: JSON-RPC メッセージのラウンドトリップ (Playwright)

#### 13c.2 User Consent ダイアログ

- [x] `src/components/modals/IpcConsentDialog.tsx`: IPC 許可リクエストダイアログ
  - Display mode 別アイコン (Widget/App/Headless)
  - 許可 / 拒否 ボタン、Escape で拒否
  - gb-\* デザインシステム準拠
- [x] `HostBridgeFrame.tsx` から `capsule/ui.modeChange` 受信時に `request_owner_consent` 呼び出し
- [x] セキュリティ: Headless → App 直接昇格禁止 (`-32001 Permission denied`)
- [x] `src-tauri/src/commands/guest.rs`: `guest_validate_mode_change` Tauri Command
  - Headless → App 直接ブロック
  - De-escalation は自動承認
  - Escalation は owner consent 経由
- [ ] テスト: ダイアログ表示・許可・拒否の E2E (Playwright)

#### 13c.3 Display Mode 管理

- [x] `src/components/session/GuestWidget.tsx`: Widget PiP コンポーネント
  - Widget mode: ドラッグ可能な浮遊ウィンドウ + タイトルバー
  - App mode: フルスクリーンコンテナ + minimize/close ボタン
  - Headless mode: レンダリングなし
  - placement 設定: top-right/left, bottom-right/left, center
- [x] `src/hooks/useGuestIpc.ts`: Guest IPC セッション状態管理
  - DisplayMode 管理 (widget/app/headless)
  - Mode transition validation (escalation/de-escalation)
  - WidgetConfig (width/height/placement/allowModeTransitions)
  - Consent flow integration
- [x] Rust 型: `src-tauri/src/ipc/guest.rs`
  - `GuestDisplayMode`, `GuestIpcEnv`, `WidgetConfig`
  - `GuestModeChangeRequest`, `GuestModeChangeResult`
  - テスト 5件
- [ ] Headless モード: Sidebar にステータスインジケータ
- [ ] テスト: Widget 表示・リサイズ・モード切替

#### 13c.4 Host-Guest 環境変数注入

- [x] `src-tauri/src/commands/guest.rs`: `guest_get_ipc_env` Tauri Command
  - `CAPSULE_IPC_PROTOCOL`, `CAPSULE_IPC_TRANSPORT`, `CAPSULE_SESSION_ID` 生成
  - collect_commands! に登録済み
- [x] `HostBridgeFrame.tsx`: `capsule/initialize` 応答に `ipc_env` を含む
- [ ] テスト: Guest が環境変数を受け取れること

---

### 13d: E2E 統合テスト (見積 1週間)

#### 13d.1 Source Runtime IPC E2E

- [x] サンプル: greeter-service (Source) + greeter-client (Source)
  - `samples/greeter-service/` — capsule.toml + server.js + README.md
  - `samples/greeter-client/` — capsule.toml + client.js
- [x] `ato ipc status` — 空状態で "No IPC services running" (テスト: `ipc_status_shows_no_services`)
- [x] `ato ipc status --json` — 空配列返却 (テスト: `ipc_status_json_returns_empty_array`)
- [x] `ato ipc start` — fixture からサービス登録 (テスト: `ipc_start_registers_service`)
- [x] `ato ipc start --json` — 有効な JSON 出力 (テスト: `ipc_start_json_output_is_valid`)
- [x] `ato ipc stop` — not found 時のエラー表示 (テスト: `ipc_stop_reports_not_found`)
- [x] Start → Stop ラウンドトリップ (テスト: `ipc_start_then_stop_roundtrip`)
- [ ] my-app 終了後、idle_timeout 後に greeter-service が停止すること (ランタイム統合時)

#### 13d.2 Cross-Runtime IPC E2E

- [ ] サンプル: llm-service (OCI) + my-app (Source)
- [ ] サンプル: wasm-service (Wasm) + my-app (Source)

#### 13d.3 Guest Mode IPC E2E

- [x] JSON-RPC 2.0 capsule/ping ラウンドトリップ (Playwright: `guest-ipc.spec.ts`)
- [x] JSON-RPC エラーレスポンスフォーマット (-32601)
- [x] capsule/initialize 応答コントラクト検証
- [x] Mode change escalation ルール検証 (widget→app consent必須)
- [x] `-32001 Permission denied` エラーフォーマット
- [x] Lifecycle shutdown プロトコル (通知→ready→ack)

#### 13d.4 エラー系 E2E

- [x] capsule.toml 未存在 → `ato ipc start` 失敗 (テスト: `ipc_start_fails_without_capsule_toml`)
- [x] [ipc] セクション無し → フォールバック名で登録 (テスト: `ipc_start_with_no_ipc_section_uses_fallback_name`)
- [x] 未登録 Service → `ato ipc stop` で not_found (テスト: `ipc_stop_json_reports_not_found`)
- [ ] Schema 違反 → `-32003` + hint (validate.rs 内部テストで検証済み: 97件)
- [ ] `ato validate` CLI サブコマンド追加 (将来)

#### テスト結果サマリー

| テストスイート       | 合格    | 場所                                                     |
| -------------------- | ------- | -------------------------------------------------------- |
| IPC CLI E2E          | 12/12   | `tests/ipc_e2e.rs`                                       |
| IPC ユニットテスト   | 136/136 | `src/ipc/*.rs` (validate, jsonrpc, schema, broker, etc.) |
| ato-desktop Rust     | 73/73   | `src-tauri/src/**`                                       |
| Guest IPC Playwright | 6 specs | `tests/e2e/guest-ipc.spec.ts`                            |

#### テスト Fixtures

| ディレクトリ                   | 用途                           |
| ------------------------------ | ------------------------------ |
| `tests/fixtures/ipc_service/`  | 正常な IPC サービス            |
| `tests/fixtures/ipc_client/`   | 正常な IPC クライアント        |
| `tests/fixtures/ipc_conflict/` | IPC-001 sharing+lifecycle 競合 |
| `tests/fixtures/ipc_reserved/` | IPC-002 予約プレフィックス     |
| `tests/fixtures/ipc_circular/` | IPC-003 循環依存               |

### 13e: サンプルアプリ

- [x] `samples/greeter-service/` — 最小の Shared Service サンプル (Node.js JSON-RPC 2.0 server)
- [x] `samples/greeter-client/` — greeter-service を利用する Client (Node.js)
- [x] `samples/greeter-service/README.md` — IPC Quick Start + 通信フロー図
- [ ] `samples/cross-runtime-demo/` — OCI Service + Source Client のデモ

---

## Phase 14: Specification Tasks 📋

**目的**: 新機能・改善の仕様策定とサンプル実装

### 14.1 Desktop Tab Management 仕様策定

**仕様**: [DESKTOP_TAB_SPEC.md](docs/specs/DESKTOP_TAB_SPEC.md)

**背景**: タブ切り替え時に各カプセルの状態（フォーム入力・スクロール位置等）を保持し、ブラウザ的なUXを実現するためのマルチWebViewアーキテクチャの策定。

**タスク**:

- [ ] **調査・設計**: Tauri v2 Multi-Webview調査、メモリ負荷分析
- [ ] **仕様書作成**: 三層アーキテクチャ（WebView管理・ガバナンス・UI）の詳細仕様
- [ ] **プロトタイプ**: Resource GovernorとScreenshot管理のPoC
- [ ] **実装**:
  - `WebViewManager`（Multi-Webview制御）
  - `ResourceGovernor`（メモリ監視・Freeze/Kill判断）
  - Screenshotキャッシュシステム
  - Frontend統合（React hooks, UI components）

**設計方針**:

```
Layer 1: Multi-Webview (Tauri v2) — プロセス分離、OSメモリ管理委譲
Layer 2: Resource Governance (Rust) — CPU/RAM監視、動的タブ管理
Layer 3: UI (React) — タブ表示、スクリーンショット表示、状態管理
```

**状態遷移**: Active → Frozen (OS圧縮) → Suspended (Screenshot) → Killed (破棄)

---

### 14.2 Capsule Icon 仕様策定

**背景**: capsule.tomlでアプリアイコンを指定できる仕様の策定。タブバーやアプリリストでの視覚的識別を可能にする。

**タスク**:

- [ ] **仕様策定**:
  - `[metadata.icon]` セクションのスキーマ定義
  - サポートフォーマット: SVG（推奨）/ PNG / JPEG
  - 複数サイズ対応（32/64/128px）
  - Data URI対応（小さなアイコン向け）
- [ ] **スキーマ定義**:

```toml
[metadata.icon]
path = "assets/icon.svg"              # 方式1: 単一ファイル
type = "svg"

[[metadata.icon.sources]]             # 方式2: 複数サイズ
size = 32
path = "assets/icon-32x32.png"

[[metadata.icon.sources]]
size = 128
path = "assets/icon-128x128.png"
```

- [ ] **実装**:
  - `ato-cli`: アイコンパス解決・検証
  - `ato-desktop`: アイコンローダー（`loadCapsuleIcon()`）
  - UI統合: タブバー、アプリリスト、アソシエーションダイアログ
- [ ] **バリデーション**: ファイル存在確認、サイズ上限（100KB）、推奨フォーマット警告

---

### 14.3 SQLiteメモ・TODOアプリ開発

**背景**: sync.wasm + SQLiteを使ったローカルファーストのメモ・TODOアプリ。docker-compose的な複数Services構成でnacelle上で動作。

**アーキテクチャ**:

```
[services.db]      # SQLite (Prisma経由)
[services.api]     # Backend API (Node.js/Python)
[services.web]     # Frontend (React)
depends_on = ["db", "api"]
```

**タスク**:

- [ ] **仕様策定**:
  - データモデル（notes, todos, attachments, notes_fts）
  - API設計（CRUD, 検索, エクスポート）
  - UI/UX（リスト・エディタ・検索・タグ）

- [ ] **データベース設計**:

```sql
CREATE TABLE notes (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    content TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    archived BOOLEAN DEFAULT FALSE,
    color TEXT DEFAULT '#ffffff',
    tags TEXT -- JSON array
);

CREATE VIRTUAL TABLE notes_fts USING fts5(title, content);
```

- [ ] **実装**:
  - Backend: REST API（FastAPI/Express）
  - Frontend: React + TailwindCSS + TipTap（リッチテキスト）
  - Database: SQLite + Prisma + FTS5全文検索
  - capsule.toml: Multi-services構成
- [ ] **機能**:
  - メモ作成・編集・削除
  - TODOリスト（優先度、期限）
  - タグ付け・フィルタリング
  - 全文検索（FTS5）
  - データエクスポート（Markdown/JSON）
  - オフライン対応（local-first）

- [ ] **デプロイ**:
  - `samples/notes-app/` として配置
  - README（Quick Start, スクリーンショット）

---

### 14.4 OSSアプリCapsul化スコープ仕様

**背景**: GitHubにある既存OSSアプリをcapsule.tomlだけで動作させるための互換性スコープと境界の定義。最小限の変更でCapsule化できる範囲を明確化。

**タスク**:

- [ ] **対応範囲定義**:

| レベル                      | 説明                   | 例                                   |
| --------------------------- | ---------------------- | ------------------------------------ |
| **Level 1: Zero-Code**      | capsule.tomlのみで動作 | React SPA, Vue SPA, Express API      |
| **Level 2: Build Required** | JIT Provisioning必要   | Next.js, Viteビルド                  |
| **Level 3: Adaptation**     | 環境変数・パス変更のみ | Django, Rails                        |
| **Level 4: Not Compatible** | コード変更必須         | Electron, React Native, カーネル依存 |

- [ ] **互換性マトリクス作成**:

| Framework    | 互換性     | 条件                       |
| ------------ | ---------- | -------------------------- |
| React SPA    | 🟢 Full    | Build step may be needed   |
| Vue SPA      | 🟢 Full    | Static export対応          |
| Next.js      | 🟡 Partial | SSR needs special handling |
| Express      | 🟢 Full    | Network config required    |
| FastAPI      | 🟢 Full    | Python runtime             |
| Django       | 🟡 Partial | DB migration needed        |
| Electron     | 🔴 Limited | GUI framework conflict     |
| React Native | 🔴 None    | Mobile-only                |

- [ ] **自動検出ツール**:

  ```bash
  ato detect ./my-app
  # 出力: 互換性スコア(0-100)、必要変更リスト、生成capsule.toml
  ```

- [ ] **ガイドライン作成**:
  - `docs/guides/OSS_COMPATIBILITY.md`
  - よくあるパターンと対処法
  - Dockerfileからの移行ガイド
  - トラブルシューティング

- [ ] **実例作成**:
  - 人気OSSアプリ5選のCapsul化例
  - 各レベルのサンプル実装
  - Before/After比較

**設計原則**:

- Smart Build, Dumb Runtime（宣言的構成）
- ゲスト改修最小化（Zero-Codeを目指す）
- 段階的対応（簡単なものから順次）

---

## 🧹 リファクタ系タスク

リファクタ・技術的負債解消系のタスクは [REFACTOR_TODO.md](./REFACTOR_TODO.md) に分離。

- core クレートのエラー設計リファクタ (anyhow 境界の整理) — 2026-04-23 追加
