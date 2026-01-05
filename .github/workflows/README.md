# GitHub Actions Workflows

このディレクトリには、capsuledプロジェクトのCI/CDワークフローが含まれています。

## ワークフロー一覧

### 1. `ci.yml` - メインCI

**トリガー**: `main` または `origin` ブランチへのpush/PR

**ジョブ**:

#### `test` - テストスイート
- **OS**: Ubuntu, macOS
- **実行内容**:
  - コードフォーマットチェック (`cargo fmt`)
  - Clippy linter (`cargo clippy`)
  - ビルド
  - ユニットテスト
  - 統合テスト（non-ignored）

#### `source-runtime-e2e` - Source Runtimeエンドツーエンドテスト
- **OS**: Ubuntu, macOS
- **Runtime Matrix**:
  - Python 3
  - Node.js
  - Ruby
  - Deno
- **実行内容**:
  - 各ランタイムのインストール
  - ランタイム固有のE2Eテスト実行
  - 環境変数のテスト
  - スクリプト実行のテスト

#### `security` - セキュリティ監査
- **OS**: Ubuntu
- **実行内容**:
  - `cargo audit` による依存関係の脆弱性スキャン

#### `build-release` - リリースビルド
- **OS**: Ubuntu, macOS
- **実行内容**:
  - リリースビルド (`--release`)
  - バイナリのアーティファクトアップロード

### 2. `quick-check.yml` - クイックチェック

**トリガー**: `main`/`origin` 以外のブランチへのpush、または全てのPR

**ジョブ**:
- フォーマットチェック
- Clippy
- ビルド
- ユニットテスト（統合テストはスキップ）

軽量で高速なフィードバックを提供します。

## ローカルでのテスト実行

### 全テスト実行
```bash
cargo test --verbose
```

### Source Runtime E2Eテスト

#### Python
```bash
cargo test --test source_runtime_e2e -- --ignored python --nocapture
```

#### Node.js
```bash
cargo test --test source_runtime_e2e -- --ignored node --nocapture
```

#### Ruby
```bash
cargo test --test source_runtime_e2e -- --ignored ruby --nocapture
```

#### Deno
```bash
cargo test --test source_runtime_e2e -- --ignored deno --nocapture
```

#### 全てのSource Runtime E2E
```bash
cargo test --test source_runtime_e2e -- --ignored --nocapture
```

## 事前準備

### 必要なツール

#### Ubuntu/Debian
```bash
sudo apt-get update
sudo apt-get install -y protobuf-compiler python3 nodejs npm ruby

# Deno
curl -fsSL https://deno.land/install.sh | sh
```

#### macOS
```bash
brew install protobuf python3 node ruby deno
```

## キャッシュ戦略

ワークフローは以下をキャッシュして高速化しています:
- Cargo registry (`~/.cargo/registry`)
- Cargo git index (`~/.cargo/git`)
- ビルドアーティファクト (`target/`)

## トラブルシューティング

### Protobuf compiler not found
```bash
# Ubuntu
sudo apt-get install -y protobuf-compiler

# macOS
brew install protobuf
```

### Runtime not found
各ランタイムがシステムにインストールされていることを確認してください:
```bash
python3 --version
node --version
ruby --version
deno --version
```

### Tests failing locally but passing in CI
- キャッシュをクリアしてみてください: `cargo clean`
- 環境変数が正しく設定されているか確認してください
- ローカルのランタイムバージョンがCI環境と一致しているか確認してください

## 拡張

新しいランタイムを追加する場合:

1. `tests/source_runtime_e2e.rs` に新しいテストモジュールを追加
2. `ci.yml` の `source-runtime-e2e` ジョブに新しいランタイムマトリックスを追加
3. インストールコマンドとテストフィルタを設定

例:
```yaml
- name: go
  install-ubuntu: "sudo apt-get install -y golang"
  install-macos: "brew install go"
  test-filter: "golang"
```
