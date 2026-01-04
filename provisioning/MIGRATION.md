# Migration Guide: Scripts & Systemd → Provisioning

このガイドでは、古い`scripts/`と`systemd/`ディレクトリから新しい`provisioning/`構造への移行方法を説明します。

## 📋 変更の概要

### 削除可能なディレクトリ

以下のディレクトリは、`provisioning/`に統合されたため**削除可能**です：

```bash
scripts/            # → provisioning/scripts/ に統合
systemd/            # → manage_services.py で動的生成
```

### 新しい構造

```
provisioning/
├── scripts/
│   ├── deploy.py              # Phase 0セットアップ（既存）
│   ├── cleanup.py             # クリーンアップ（既存）
│   ├── manage_services.py     # 🆕 systemdサービス管理
│   ├── manage_api_keys.py     # 🆕 API鍵管理
│   ├── requirements.txt
│   └── tests/
├── cloud-init/
│   └── user-data.yml
└── systemd/                   # manage_services.pyで自動生成
    ├── caddy.service
    ├── rig-manager.service
    ├── rig-reconciler.service
    ├── rig-reconciler.timer
    ├── rig-cleanup.service
    └── rig-cleanup.timer
```

## 🔄 機能マッピング

### 1. API 鍵生成

#### 旧方式 (scripts/generate_api_key.sh)

```bash
./scripts/generate_api_key.sh
```

#### 新方式 (manage_api_keys.py)

```bash
python3 provisioning/scripts/manage_api_keys.py generate

# その他の機能
python3 provisioning/scripts/manage_api_keys.py verify <api_key>
python3 provisioning/scripts/manage_api_keys.py hash <api_key>
```

### 2. systemd サービス管理

#### 旧方式 (systemd/ ディレクトリ)

```bash
sudo cp systemd/rig-manager.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable rig-manager.service
```

#### 新方式 (manage_services.py)

```bash
# 利用可能なサービス一覧
python3 provisioning/scripts/manage_services.py list

# サービスファイルを生成
python3 provisioning/scripts/manage_services.py generate

# サービスをインストール
python3 provisioning/scripts/manage_services.py install

# サービスを有効化
python3 provisioning/scripts/manage_services.py enable

# 全て一括実行
python3 provisioning/scripts/manage_services.py all
```

### 3. デプロイメント

#### 旧方式 (scripts/deploy-to-rig.sh)

```bash
./scripts/deploy-to-rig.sh
```

#### 新方式 (deploy.py)

```bash
# 完全セットアップ
python3 provisioning/scripts/deploy.py

# deploy.pyは自動的に以下を実行：
# - youki, Rust, Caddyのインストール
# - systemdサービスの生成とインストール
# - データベースマイグレーション
```

### 4. OCI 環境セットアップ

#### 旧方式 (scripts/setup_oci_rig.sh)

```bash
sudo bash scripts/setup_oci_rig.sh --tailscale-up
```

#### 新方式 (deploy.py + 追加設定)

```bash
# Phase 0基盤セットアップ
python3 provisioning/scripts/deploy.py

# Tailscaleは手動で追加（またはdeploy.pyに統合可能）
curl -fsSL https://tailscale.com/install.sh | sh
sudo tailscale up --ssh
```

## 🗑️ 安全な削除手順

### ステップ 1: バックアップ

```bash
# 念のため旧ディレクトリをバックアップ
cp -r scripts/ scripts.backup/
cp -r systemd/ systemd.backup/
```

### ステップ 2: 新しいツールのテスト

```bash
# API鍵生成テスト
python3 provisioning/scripts/manage_api_keys.py generate

# systemdサービス生成テスト
python3 provisioning/scripts/manage_services.py generate

# サービス一覧確認
python3 provisioning/scripts/manage_services.py list
```

### ステップ 3: 移行確認チェックリスト

- [ ] API 鍵生成が動作する
- [ ] systemd サービスが正しく生成される
- [ ] deploy.py が正常に実行できる
- [ ] 既存のワークフローが新しいスクリプトで動作する

### ステップ 4: 旧ディレクトリの削除

```bash
# 全て確認できたら削除
rm -rf scripts/
rm -rf systemd/

# または .gitignore に追加して非表示に
echo "scripts/" >> .gitignore
echo "systemd/" >> .gitignore
```

## 📝 新しいワークフロー例

### 新規 OCI VM のセットアップ

```bash
# 1. リポジトリクローン
git clone <repository-url>
cd rig-manager

# 2. 依存関係インストール
pip3 install -r provisioning/scripts/requirements.txt

# 3. Phase 0セットアップ実行
python3 provisioning/scripts/deploy.py

# 4. API鍵生成
python3 provisioning/scripts/manage_api_keys.py generate

# 5. 追加サービスのインストール（必要に応じて）
python3 provisioning/scripts/manage_services.py install \
  --services rig-manager.service rig-reconciler.timer rig-cleanup.timer

# 6. サービス有効化
python3 provisioning/scripts/manage_services.py enable \
  --services rig-manager.service rig-reconciler.timer rig-cleanup.timer

# 7. サービス起動確認
sudo systemctl status rig-manager
```

### 開発中のワークフロー

```bash
# ビルド
cargo build --release

# デプロイ（Rustバイナリを手動配置）
sudo cp target/release/rig-manager /usr/local/bin/

# サービス再起動
sudo systemctl restart rig-manager

# ログ確認
sudo journalctl -u rig-manager -f
```

## 🔧 トラブルシューティング

### 問題: 旧スクリプトへの依存がある

**解決策**:

1. 依存している箇所を特定
2. `provisioning/scripts/`の対応するツールに置き換え
3. 必要に応じて新しい Python スクリプトを追加

### 問題: systemd サービスが見つからない

**解決策**:

```bash
# サービスファイルを再生成
python3 provisioning/scripts/manage_services.py generate

# インストール
python3 provisioning/scripts/manage_services.py install
```

### 問題: API 鍵の形式が異なる

**解決策**:
新しい`manage_api_keys.py`は旧スクリプトと同じ形式（base64 + SHA256）を使用しています。
既存の`.env`ファイルはそのまま使用できます。

## 📚 関連ドキュメント

- [provisioning/README.md](README.md) - 詳細な使用方法
- [manage_services.py](scripts/manage_services.py) - systemd サービス管理
- [manage_api_keys.py](scripts/manage_api_keys.py) - API 鍵管理
- [deploy.py](scripts/deploy.py) - セットアップスクリプト

## ✅ 移行完了後の確認事項

- [ ] 旧`scripts/`の機能が全て`provisioning/scripts/`で実行できる
- [ ] 旧`systemd/`のサービスが全て`manage_services.py`で生成できる
- [ ] CI/CD パイプラインが新しいパスを参照している
- [ ] ドキュメントが更新されている
- [ ] チーム全体が新しいワークフローを理解している

移行に関する質問や問題がある場合は、Issue を作成してください。
