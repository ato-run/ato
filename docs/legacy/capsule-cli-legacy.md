# Capsule CLI

**Universal Application Runtime Contract Manager** - UARC V1.1.0 準拠の CLI ツール

[![UARC](https://img.shields.io/badge/UARC-V1.1.0-blue)](../../uarc/SPEC.md)

## Overview

Capsule CLI は [UARC (Universal Application Runtime Contract)](../../uarc/SPEC.md) 仕様に準拠したアプリケーションのビルド、パッケージング、デプロイを行うコマンドラインツールです。

## インストール

```bash
# ソースからビルド
cd capsuled
cargo build -p capsule-cli --release

# バイナリをPATHに追加
cp target/release/capsule ~/.local/bin/
```

## コマンド一覧

```
Capsule CLI - Universal Application Runtime Contract Manager

LIFECYCLE:
  new      Create a new Capsule project
  init     Initialize existing project as Capsule
  open     Open and launch a Capsule (--dev for development)
  close    Close a running Capsule
  logs     Stream logs from an open Capsule
  ps       List currently open Capsules

PACKAGING:
  pack     Build and sign a .capsule archive
  keygen   Generate developer signing keys

SYSTEM:
  doctor   Check Engine status and host requirements
```

## クイックスタート

### 新規プロジェクト作成

```bash
# Python プロジェクトを作成
capsule new my-app --template python

# 利用可能なテンプレート: python, node, rust, shell
capsule new my-api --template node
```

### 既存プロジェクトの Capsule 化

```bash
cd my-existing-project

# プロジェクトタイプを自動検出して capsule.toml を生成
capsule init

# 確認プロンプトをスキップ
capsule init --yes
```

### 開発モードで実行

```bash
# 開発モード（ホットリロード、セキュリティ緩和）
capsule open --dev

# ログを確認
capsule logs <capsule-id>

# 終了
capsule close <capsule-id>
```

### 本番パッケージング

```bash
# 署名キーを生成（初回のみ）
capsule keygen --name production

# パッケージと署名
capsule pack --key ~/.capsule/keys/production.secret

# 本番モードで実行
capsule open my-app.capsule
```

## コマンド詳細

### `capsule new`

新しい Capsule プロジェクトをテンプレートから作成します。

```bash
capsule new <NAME> [OPTIONS]

OPTIONS:
  -t, --template <TEMPLATE>  テンプレート種別 [default: python]
                             利用可能: python, node, rust, shell
```

**生成されるファイル:**

- `capsule.toml` - Capsule マニフェスト
- エントリポイントファイル (`main.py`, `index.js`, etc.)
- `.gitignore`
- `README.md`

### `capsule init`

既存プロジェクトを Capsule として初期化します。

```bash
capsule init [OPTIONS]

OPTIONS:
  -p, --path <PATH>  対象ディレクトリ [default: .]
  -y, --yes          対話プロンプトをスキップ
```

**自動検出される言語:**

- Python (`requirements.txt`, `pyproject.toml`)
- Node.js (`package.json`)
- Rust (`Cargo.toml`)
- Go (`go.mod`)
- Ruby (`Gemfile`)

### `capsule open`

Capsule を起動します。

```bash
capsule open [PATH] [OPTIONS]

ARGUMENTS:
  [PATH]  capsule.toml または .capsule ファイル [default: capsule.toml]

OPTIONS:
  -d, --dev          開発モード（セキュリティ緩和）
```

**開発モード vs 本番モード:**

| モード       | 署名 | セキュリティ | 用途                   |
| ------------ | ---- | ------------ | ---------------------- |
| `--dev`      | 不要 | 緩和         | ローカル開発、デバッグ |
| (デフォルト) | 推奨 | フル         | 本番デプロイ           |

### `capsule close`

実行中の Capsule を終了します。

```bash
capsule close <CAPSULE_ID>
```

### `capsule ps`

実行中の Capsule を一覧表示します。

```bash
capsule ps [OPTIONS]

OPTIONS:
  -a, --all  停止済みも含めて表示
```

### `capsule logs`

Capsule のログをストリーミング表示します。

```bash
capsule logs <CAPSULE_ID> [OPTIONS]

OPTIONS:
  -f, --follow  リアルタイムフォロー (tail -f 相当)
```

### `capsule pack`

Capsule をパッケージング（と署名）します。

```bash
capsule pack [OPTIONS]

OPTIONS:
  -m, --manifest <PATH>  マニフェストファイル [default: capsule.toml]
  -o, --output <PATH>    出力ファイルパス
  -k, --key <PATH>       署名キー (.secret ファイル)
```

**出力:**

- `<name>.capsule` - パッケージ済みマニフェスト (JSON)
- `<name>.sig` - Ed25519 署名 (--key 指定時)

### `capsule keygen`

Ed25519 署名キーペアを生成します。

```bash
capsule keygen [OPTIONS]

OPTIONS:
  -n, --name <NAME>  キー名 [default: timestamp-based]
```

**保存場所:** `~/.capsule/keys/`

- `<name>.secret` - 秘密鍵 (0600 パーミッション)
- `<name>.public` - 公開鍵

### `capsule doctor`

システム診断を実行します。

```bash
capsule doctor [OPTIONS]

OPTIONS:
  -v, --verbose  詳細情報を表示
```

**チェック項目:**

- ✅ Engine 接続状態
- ✅ 署名キーの存在
- ✅ Python, Node.js, Docker の可用性
- ✅ GPU (Apple Silicon MPS / NVIDIA CUDA)

## マニフェスト形式

`capsule.toml` の基本構造:

```toml
# Capsule Manifest - UARC V1.1.0
schema_version = "1.0"
name = "my-app"
version = "0.1.0"
type = "app"

[metadata]
description = "My awesome application"

[requirements]

[execution]
runtime = "source"
entrypoint = "python main.py"

[storage]

[routing]
```

## Engine 接続

CLI は capsuled Engine (gRPC サーバー) と通信して Capsule を管理します。

```bash
# デフォルト接続先
# http://127.0.0.1:50051

# 環境変数で変更
export CAPSULE_ENGINE_URL=http://192.168.1.100:50051
capsule open --dev

# コマンドラインで指定
capsule --engine-url http://192.168.1.100:50051 open --dev
```

## 関連プロジェクト

- [capsuled](../) - Capsule Engine (ランタイム)
- [uarc](../../uarc/) - UARC 仕様
- [ato-desktop](../../ato-desktop/) - デスクトップ UI

## ライセンス

FSL-1.1-ALv2 (Functional Source License 1.1, Apache License Version 2.0)
