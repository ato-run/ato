# Ato: The Agentic Meta-Runtime 🚀

[![CI](https://github.com/ato-run/ato-cli/actions/workflows/build-multi-os.yml/badge.svg?branch=dev)](https://github.com/ato-run/ato-cli/actions/workflows/build-multi-os.yml)
[![GitHub Release](https://img.shields.io/github/v/release/ato-run/ato-cli)](https://github.com/ato-run/ato-cli/releases)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange?logo=rust)](https://www.rust-lang.org/)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-6f42c1)
[![Issues](https://img.shields.io/github/issues/ato-run/ato-cli)](https://github.com/ato-run/ato-cli/issues)
[![Last Commit](https://img.shields.io/github/last-commit/ato-run/ato-cli/dev)](https://github.com/ato-run/ato-cli/commits/dev)

日本語 | [English](README.md)

> AI 時代の Nix 代替。URL を渡すだけで、安全な実行環境を数秒で立ち上げるメタランタイム。

Ato は、重いコンテナビルドや独自 DSL をユーザーに強いません。ソースから必要なランタイムと依存関係を推論し、必要なぶんだけ具現化し、Fail-Closed を前提に実行します。Python やネイティブ実行のような Tier 2 ターゲットでは、Nacelle サンドボックスを使い、通常のローカル実行では必要になった時点で互換エンジンの自動ブートストラップも試みます。

この README は MVP の主導線である `run`、`encap`、`decap` に集中しています。ストアや publish 系の高度なフローはここでは扱いません。

---

## 3つのコマンド

### 1. まず試す: `ato run`

`ato run` は、共有 URL、GitHub リポジトリ、ローカルのスクリプトを、手元の環境を汚さずにそのまま実行します。依存関係の解決は隔離された経路で行われ、実行結果は基本的に Ephemeral なものとして扱われます。

```bash
# 共有ワークスペースをそのまま実行
ato run https://ato.run/s/demo@r1

# GitHub リポジトリをそのまま実行
ato run github.com/user/my-app

# ローカルの単一スクリプトを実行
ato run scrape.py
```

まず動かしたいなら `ato run`、ファイル一式をローカルに展開したいなら `ato decap` を使います。

### 2. ワークスペースを共有する: `ato encap`

`ato encap` は、現在のワークスペースをポータブルな共有ディスクリプタとしてキャプチャし、ローカルの share ファイルを書き出し、必要なら共有 URL へアップロードします。

```bash
# 現在のワークスペースをキャプチャしてアップロード
ato encap . --share
# -> https://ato.run/s/myproject@r1
```

ローカルには `.ato/share/` 以下が書き出されます。

- `share.spec.json`
- `share.lock.json`
- `guide.md`

保存されるのは環境変数ファイルの要件や実行契約であって、シークレットの値そのものではありません。

### 3. ローカルへ復元する: `ato decap`

`ato decap` は、共有ワークスペースを指定ディレクトリへ具現化します。単なるアーカイブ展開ではなく、共有内容の検証を行い、宣言されたインストールステップまで実行して、再現可能なローカル環境を組み立てます。

```bash
# 共有 URL から具現化
ato decap https://ato.run/s/myproject@r1 --into ./my-project

# ローカルの share ディスクリプタから具現化
ato decap .ato/share/share.spec.json --into ./my-project
```

---

## Ato が得意な入力とランタイム

Ato の推論エンジンは、たとえば次のようなケースをうまく扱えます。

- `https://ato.run/s/...` 形式の共有 URL
- `github.com/owner/repo` 形式の GitHub リポジトリ
- PEP 723 メタデータを含む単一ファイル Python スクリプト
- `deno.json`、`package.json`、各種 lockfile から推論できる TypeScript / JavaScript プロジェクト
- `pyproject.toml` や `uv.lock` を持つ Python プロジェクト
- Rust、Go、Static Web、WebAssembly などの lock-first なプロジェクト構成

再現可能な実行経路を Ato が判定できる場合、どの入力も同じ capsule 指向の実行モデルに載せます。

---

## クイックスタート

まずは 1 行インストーラーで導入できます。

```bash
curl -fsSL https://ato.run/install.sh | sh
```

あるいは [GitHub Releases ページ](https://github.com/ato-run/ato-cli/releases/latest) からバイナリを取得し、`PATH` に置いてください。

コントリビューター向けにソースから試すなら次の流れです。

```bash
# CLI をビルド
cargo build -p ato-cli

# 現在のディレクトリを実行
./target/debug/ato run .

# 開発時の watch 実行
./target/debug/ato run . --watch

# バックグラウンド実行と管理
./target/debug/ato run . --background
./target/debug/ato ps
./target/debug/ato stop --id <capsule-id>
./target/debug/ato logs --id <capsule-id> --follow
```

---

## 主要コマンドリファレンス

デフォルトの CLI help は、最小の主導線だけを前面に出しています。

```text
Usage: ato [OPTIONS] <COMMAND>

Primary Commands:
  run      Try something now
  decap    Set up a workspace locally
  encap    Share your current workspace

Management:
  ps       List running capsules
  stop     Stop a running capsule
  logs     Show logs of a running capsule
```

---

## セキュリティモデル

Ato はデフォルトで Fail-Closed です。

- サンドボックス隔離: Tier 2 ターゲットは [Nacelle](https://github.com/ato-run/nacelle) 経由で実行します。
- ファイルシステム保護: 未知のコードに無制限のホスト権限を与えません。
- ネットワーク制御: strict enforcement では未許可の通信を遮断します。
- 環境変数管理: 必須の環境変数が欠けていれば起動前に停止し、`--prompt-env` で対話入力もできます。

通常のローカル実行では、Tier 2 実行に Nacelle が必要になった時点で Ato が互換バージョンの自動ブートストラップを試みます。CI やオフライン環境ではこの自動取得が制限されるため、その場合は事前に Nacelle を導入または登録してください。

---

## ランタイム隔離 Tier

ランタイムの種類によって必要な隔離レベルは異なります。

| ランタイム系 | Tier | 補足 |
| --- | --- | --- |
| `web/static` | Tier 1 | Static preview やシンプルな Web ターゲット |
| `web/deno`, `web/node`, `source/deno`, `source/node` | Tier 1 | 通常の経路では手動サンドボックス準備なしで実行 |
| `source/python`, `web/python`, `source/native` | Tier 2 | Nacelle が必要。CI やオフライン以外では通常自動ブートストラップ対象 |

Tier 1 は権限バイパス用の特別なフラグなしで実行します。Tier 2 はより強いサンドボックス経路を通ります。

---

## コントリビュート

バグ報告や機能要望は [GitHub Issues](https://github.com/ato-run/ato-cli/issues) で受け付けています。

## License

Apache License 2.0 (SPDX: Apache-2.0). See [LICENSE](LICENSE).
