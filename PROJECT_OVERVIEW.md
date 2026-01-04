# Capsuled Engine - Project Overview

**最終更新:** 2025年11月15日
**バージョン:** 0.1.0
**ステータス:** Phase 1 開発中

> **Note**: このドキュメントは Engine コンポーネントの歴史的な開発記録として保持されています。
> 最新のアーキテクチャ情報については、リポジトリルートの [ARCHITECTURE.md](../ARCHITECTURE.md) を参照してください。

---

## 📋 目次

1. [プロジェクト概要](#プロジェクト概要)
2. [アーキテクチャ](#アーキテクチャ)
3. [ディレクトリ構造](#ディレクトリ構造)
4. [主要コンポーネント](#主要コンポーネント)
5. [技術スタック](#技術スタック)
6. [セットアップ](#セットアップ)
7. [開発ワークフロー](#開発ワークフロー)
8. [デプロイメント](#デプロイメント)
9. [ロードマップ](#ロードマップ)

---

## 🎯 プロジェクト概要

### 目的

**Capsuled Engine** は、Capsuled 分散システムの実行エージェントです。Coordinator（Client コンポーネント）からの指示を受けて、OCI互換のコンテナランタイムを使用してカプセル（コンテナ）を実行します。各ノード上で動作し、ハードウェアリソース（特にGPU）の監視と管理を行います。

### 主な機能

- **カプセルデプロイメント**: OCI bundleのアップロード、展開、実行
- **ポート管理**: 動的ポート割り当てとクールダウン期間管理
- **リバースプロキシ統合**: Caddyを使用した自動SSL証明書取得とHTTPS公開
- **認証**: API Key認証によるセキュアなAPI通信
- **非同期処理**: ステートマシンベースのデプロイメントワークフロー
- **ログストリーミング**: WebSocket経由のリアルタイムログ配信
- **ヘルスチェック**: コンテナ状態の監視と自動調整（Reconciler）

### KPI達成基準

- [ ] Tauriクライアントから OCI Rig経由でカプセルをデプロイ
- [ ] 公開URL（`https://*.rig.my-startup.com`）経由でブラウザアクセス可能
- [ ] Tailscale VPN経由のセキュアなAPI通信
- [ ] デプロイ進捗のリアルタイム表示

---

## 🏗 アーキテクチャ

### システム構成

```
┌──────────────────┐
│  外部クライアント   │
│  (CLI/Desktop)   │
└────────┬─────────┘
         │ HTTPS API
         ▼
┌─────────────────────────────────┐
│  Capsuled Coordinator (Client)  │
│         (Go)                    │
└────────┬────────────────────────┘
         │ gRPC
         ▼
┌─────────────────────────────────┐
│   Capsuled Engine (このコンポーネント) │
│  ┌───────────────────────────┐  │
│  │  API エンドポイント        │  │
│  │  - POST /v1/deployments   │  │
│  │  - GET  /v1/deployments/:id│ │
│  │  - GET  /v1/capsules/:id  │  │
│  │  - WS   /v1/logs/:id      │  │
│  └───────────────────────────┘  │
│  ┌───────────────────────────┐  │
│  │  ストレージ管理              │  │
│  │  - LVM暗号化ボリューム      │  │
│  │  - Bundle展開              │  │
│  └───────────────────────────┘  │
│  ┌───────────────────────────┐  │
│  │  ランタイム統合              │  │
│  │  - youki (OCI Runtime)    │  │
│  └───────────────────────────┘  │
│  ┌───────────────────────────┐  │
│  │  プロキシ管理                │  │
│  │  - Caddy Admin API        │  │
│  │  - 動的ルート設定           │  │
│  └───────────────────────────┘  │
└─────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────┐
│       Caddy (Reverse Proxy)     │
│  - 自動SSL証明書取得             │
│  - HTTPS終端                    │
│  - *.rig.my-startup.com         │
└─────────────────────────────────┘
         │
         ▼
     Internet
```

### デプロイメントフロー

```
1. クライアント → API: POST /v1/deployments (OCI bundle)
   ↓
2. Rig-Manager: Bundle受信・保存 (状態: uploading)
   ↓
3. Rig-Manager: Bundle展開 (状態: extracting)
   ↓
4. Rig-Manager: youkiでコンテナ起動 (状態: starting)
   ↓
5. Rig-Manager: Caddyにルート追加 (状態: configuring_proxy)
   ↓
6. 完了 (状態: running)
   ↓
7. クライアント: WebSocketでログストリーミング
```

---

## 📁 ディレクトリ構造

```
rig-manager/
├── src/                        # Rustソースコード (~3,500 LOC)
│   ├── main.rs                 # エントリポイント・CLI
│   ├── lib.rs                  # ライブラリルート
│   ├── router.rs               # APIルーティング
│   ├── state.rs                # アプリケーション状態
│   ├── config.rs               # 設定管理
│   ├── auth/                   # 認証・認可
│   │   ├── mod.rs
│   │   └── api_key.rs          # API Key検証
│   ├── routes/                 # APIエンドポイント
│   │   ├── deployments.rs      # デプロイメントAPI
│   │   ├── capsules.rs         # カプセル管理API
│   │   └── health.rs           # ヘルスチェック
│   ├── deployments/            # デプロイメント処理
│   │   ├── handler.rs          # メインハンドラ
│   │   ├── state_machine.rs   # ステートマシン
│   │   └── models.rs           # データモデル
│   ├── ports/                  # ポート管理
│   │   └── allocator.rs        # 動的ポート割り当て
│   ├── proxy/                  # リバースプロキシ統合
│   │   ├── caddy.rs            # Caddy Admin API
│   │   └── contract.rs         # プロキシ抽象化
│   ├── runtime/                # コンテナランタイム統合
│   │   ├── youki.rs            # youki統合
│   │   └── contract.rs         # ランタイム抽象化
│   ├── storage/                # ストレージ管理
│   │   ├── lvm.rs              # LVM操作
│   │   └── mod.rs
│   ├── streaming/              # ログストリーミング
│   │   ├── log_streamer.rs     # WebSocketストリーマ
│   │   └── mod.rs
│   ├── health/                 # ヘルスチェック
│   │   ├── reconciler.rs       # 調整ループ
│   │   └── mod.rs
│   └── observability/          # 監視・メトリクス
│       ├── metrics.rs          # Prometheusメトリクス
│       └── mod.rs
│
├── migrations/                 # SQLiteマイグレーション
│   ├── 001_init.sql           # 初期スキーマ
│   ├── 002_capsules.sql       # カプセルテーブル
│   ├── 003_capsule_bundle_path.sql
│   └── 004_storage_encryption_key.sql
│
├── provisioning/               # プロビジョニングツール
│   ├── scripts/               # Python自動化スクリプト
│   │   ├── deploy.py          # Phase 0セットアップ
│   │   ├── cleanup.py         # クリーンアップ
│   │   ├── manage_services.py # systemdサービス管理
│   │   ├── manage_api_keys.py # API鍵管理
│   │   ├── requirements.txt
│   │   └── tests/
│   ├── cloud-init/            # Cloud-Init設定
│   │   └── user-data.yml
│   ├── systemd/               # systemdサービスファイル
│   │   ├── rig-manager.service
│   │   ├── rig-reconciler.service
│   │   ├── rig-reconciler.timer
│   │   ├── rig-cleanup.service
│   │   ├── rig-cleanup.timer
│   │   └── caddy.service
│   ├── INTEGRATION_SUMMARY.md # 統合完了サマリー
│   └── MIGRATION.md           # 移行ガイド
│
├── local-docs/                # プロジェクトドキュメント
│   ├── OCI_RIG_TODO_MASTER.md # マスタープラン
│   ├── OCI_RIG_TODO_PHASE0.md # Phase 0タスク
│   ├── OCI_RIG_TODO_PHASE1.md # Phase 1タスク
│   ├── OCI_RIG_TODO_PHASE2.md # Phase 2タスク
│   ├── OCI_RIG_TODO_PHASE3.md # Phase 3タスク
│   ├── PHASE1_PROGRESS.md     # 進捗トラッキング
│   ├── openapi.yaml           # API仕様
│   └── *.md                   # その他ドキュメント
│
├── tests/                     # 統合テスト
│   └── integration/
│
├── test-data/                 # テストデータ
│   ├── bundle/
│   ├── bundle.tar.gz
│   └── manifest.json
│
├── .archives/                 # アーカイブ（旧実装）
│   ├── scripts/
│   └── systemd/
│
├── Cargo.toml                 # Rust依存関係
├── config.toml.example        # 設定テンプレート
├── .env.example               # 環境変数テンプレート
├── README.md                  # 基本ドキュメント
└── PROJECT_OVERVIEW.md        # このファイル
```

---

## 🔧 主要コンポーネント

### 1. API Server (Axum)

**責任**: HTTPリクエスト処理、ルーティング、認証

- **エンドポイント**:
  - `POST /v1/deployments` - 新規デプロイメント作成
  - `GET /v1/deployments/:id` - デプロイメント状態取得
  - `GET /v1/capsules/:id` - カプセル情報取得
  - `WS /v1/logs/:id` - ログストリーミング
  - `GET /health` - ヘルスチェック

- **認証**: API Key（SHA256ハッシュ検証）

### 2. Deployment Engine

**責任**: デプロイメントステートマシン管理

- **状態遷移**:
  ```
  pending → uploading → extracting → starting →
  configuring_proxy → running → stopped
  ```

- **各フェーズの処理**:
  - `uploading`: multipart形式でOCI bundleを受信
  - `extracting`: tar.gzを展開、OCI bundle検証
  - `starting`: youkiでコンテナ起動
  - `configuring_proxy`: Caddyにルート追加
  - `running`: 正常稼働中

### 3. Port Allocator

**責任**: 動的ポート割り当てと競合回避

- **機能**:
  - ポートレンジ管理（例: 8000-9000）
  - クールダウン期間（5分）
  - 使用中ポートの追跡
  - SQLiteベースの永続化

### 4. Runtime Integration (youki)

**責任**: OCI準拠コンテナランタイム統合

- **youki操作**:
  - `youki create` - コンテナ作成
  - `youki start` - コンテナ起動
  - `youki state` - 状態確認
  - `youki delete` - コンテナ削除

### 5. Proxy Manager (Caddy)

**責任**: リバースプロキシ動的設定

- **Caddy Admin API統合**:
  - ルート追加: `POST /config/apps/http/servers/srv0/routes`
  - 設定リロード: `POST /load`
  - 自動SSL証明書取得（Let's Encrypt）

### 6. Storage Manager (LVM)

**責任**: 暗号化ストレージ管理

- **LVM操作**:
  - 論理ボリューム作成
  - LUKS暗号化
  - マウント/アンマウント
  - 容量管理

### 7. Log Streamer

**責任**: WebSocket経由のリアルタイムログ配信

- **機能**:
  - コンテナログファイル監視
  - tail -f相当の機能
  - 複数クライアント対応

### 8. Health Reconciler

**責任**: コンテナ状態の監視と調整

- **調整ループ**:
  - 5分間隔で実行（systemd timer）
  - 停止コンテナの検出
  - 自動再起動（オプション）
  - メトリクス更新

---

## 💻 技術スタック

### Backend (Rust)

| カテゴリ | ライブラリ | 用途 |
|---------|----------|------|
| Webフレームワーク | axum 0.7 | HTTP/WebSocketサーバー |
| 非同期ランタイム | tokio 1.x | 非同期処理 |
| データベース | sqlx 0.7 + SQLite | 永続化、マイグレーション |
| HTTP クライアント | reqwest 0.11 | Caddy Admin API通信 |
| シリアライズ | serde + serde_json | JSON処理 |
| CLI | clap 4.0 | コマンドライン引数 |
| ログ・トレース | tracing + tracing-subscriber | 構造化ログ |
| エラー処理 | anyhow + thiserror | エラーハンドリング |
| 暗号化 | sha2 + hex | API Key検証 |
| ファイル圧縮 | tar + flate2 | Bundle展開 |
| OCI仕様 | oci-spec 0.6 | OCI bundle検証 |

### Infrastructure

| コンポーネント | バージョン | 用途 |
|--------------|----------|------|
| youki | latest | OCI準拠コンテナランタイム |
| Caddy | v2 | リバースプロキシ、自動SSL |
| SQLite | 3.x | データベース |
| LVM2 | 2.x | ストレージ管理 |
| systemd | - | サービス管理 |
| Tailscale | latest | VPNネットワーク |

### Provisioning (Python)

| ツール | 用途 |
|-------|------|
| deploy.py | Phase 0インフラセットアップ |
| manage_services.py | systemdサービス管理 |
| manage_api_keys.py | API鍵生成・検証 |
| cleanup.py | リソースクリーンアップ |

### Development

| ツール | 用途 |
|-------|------|
| cargo | Rustビルドツール |
| sqlx-cli | データベースマイグレーション |
| pytest | Python自動化スクリプトテスト |

---

## 🚀 セットアップ

### 前提条件

- **OS**: Ubuntu 22.04+ (aarch64推奨)
- **Rust**: 1.70以上
- **Python**: 3.10以上
- **youki**: インストール済み
- **Caddy**: v2インストール済み
- **Tailscale**: セットアップ済み

### ローカル開発環境

```bash
# 1. リポジトリクローン
git clone <repository-url>
cd rig-manager

# 2. 環境変数設定
cp .env.example .env
# .envを編集してRIG_API_KEY_HASHを設定

# 3. 設定ファイル
cp config.toml.example config.toml

# 4. データベースセットアップ
cargo install sqlx-cli
sqlx database create
sqlx migrate run

# 5. API鍵生成
pip3 install -r provisioning/scripts/requirements.txt
python3 provisioning/scripts/manage_api_keys.py generate

# 6. ビルド・実行
cargo build --release
cargo run
```

### OCI VM デプロイメント

```bash
# Phase 0: インフラセットアップ
python3 provisioning/scripts/deploy.py

# API鍵生成
python3 provisioning/scripts/manage_api_keys.py generate

# サービス起動
sudo systemctl start rig-manager
sudo systemctl enable rig-manager
sudo systemctl start rig-reconciler.timer
sudo systemctl start rig-cleanup.timer

# ステータス確認
sudo systemctl status rig-manager
sudo journalctl -u rig-manager -f
```

---

## 🔄 開発ワークフロー

### ビルド

```bash
# デバッグビルド
cargo build

# リリースビルド
cargo build --release

# チェック（高速）
cargo check
```

### テスト

```bash
# ユニットテスト
cargo test

# 統合テスト
cargo test --test '*'

# Pythonスクリプトテスト
cd provisioning/scripts
pytest
```

### データベースマイグレーション

```bash
# 新規マイグレーション作成
sqlx migrate add <name>

# マイグレーション実行
sqlx migrate run

# ロールバック
sqlx migrate revert
```

### サービス管理

```bash
# サービス生成
python3 provisioning/scripts/manage_services.py generate

# サービスインストール
python3 provisioning/scripts/manage_services.py install

# サービス有効化
python3 provisioning/scripts/manage_services.py enable

# 一括実行
python3 provisioning/scripts/manage_services.py all
```

---

## 📦 デプロイメント

### systemd サービス

| サービス | 説明 | 起動タイミング |
|---------|------|---------------|
| `rig-manager.service` | メインAPIサーバー | 常時起動 |
| `rig-reconciler.timer` | 調整ループ（5分間隔） | タイマー |
| `rig-cleanup.timer` | クリーンアップ（日次） | タイマー |
| `caddy.service` | リバースプロキシ | 常時起動 |

### 設定ファイル

| ファイル | 場所 | 用途 |
|---------|------|------|
| `config.toml` | `/etc/rig-manager/production.toml` | アプリケーション設定 |
| `.env` | `/etc/rig-manager.env` | 環境変数（API Key等） |
| `rig.db` | `/var/lib/rig-manager/rig.db` | SQLiteデータベース |

### ログ

```bash
# リアルタイムログ
sudo journalctl -u rig-manager -f

# 過去ログ
sudo journalctl -u rig-manager --since "1 hour ago"

# エラーログのみ
sudo journalctl -u rig-manager -p err
```

---

## 🗺 ロードマップ

### Phase 0: インフラ準備 ✅ 完了

- [x] OCI VMプロビジョニング
- [x] Tailscaleセットアップ
- [x] youki/Caddy/Rustインストール
- [x] DNS設定

### Phase 1: Control Plane基盤 🔄 進行中 (70%)

- [x] プロジェクトセットアップ
- [x] API Key認証
- [x] ポート管理
- [x] 非同期デプロイメント（基本）
- [ ] tar.gz抽出ロジック
- [ ] ストリーミング保存

### Phase 2: Runtime統合 📅 予定

- [ ] youki統合完成
- [ ] OCI bundle展開
- [ ] コンテナライフサイクル管理
- [ ] ヘルスチェック実装

### Phase 3: Production強化 📅 予定

- [ ] LVM暗号化ストレージ
- [ ] ログストリーミング
- [ ] メトリクス収集
- [ ] エラーリカバリ

### Phase 4: クライアント統合 📅 予定

- [ ] Tauri クライアント連携
- [ ] WebSocket進捗表示
- [ ] エンドツーエンドテスト

---

## 📞 連絡先・リソース

### ドキュメント

- **マスタープラン**: `local-docs/OCI_RIG_TODO_MASTER.md`
- **API仕様**: `local-docs/openapi.yaml`
- **移行ガイド**: `provisioning/MIGRATION.md`

### リポジトリ

- **GitHub**: `https://github.com/your-org/rig-manager`

---

## 📝 ライセンス

TBD

---

**最終更新**: 2025年11月7日
**次回レビュー**: Phase 1完了時
