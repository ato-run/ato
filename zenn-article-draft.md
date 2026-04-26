# Zenn 記事下書き：ato — セットアップゼロでどんなプロジェクトも即実行する CLI

> **ステータス:** 下書き  
> **対象媒体:** Zenn  
> **文体:** 技術者向け・ですます調  
> **タグ:** Rust, CLI, DevTools, sandbox, Python

---

## ato — セットアップゼロで Python も Node も Rust も「ato run」だけで動かす

### TL;DR

```bash
curl -fsSL https://ato.run/install.sh | sh

ato run hello.py                          # Python スクリプト
ato run github.com/owner/repo             # GitHub リポジトリ
ato run https://ato.run/s/demo@r1         # シェア URL
```

virtualenv も `npm install` も Dockerfile も不要です。

---

### 背景：なぜ作ったか

他人のプロジェクトを試そうとするたびに同じことを繰り返していました。

1. README を読む
2. 指定バージョンの Python/Node を用意する
3. 依存関係をインストールする
4. 「なぜか動かない」をデバッグする

この工程が開発者間のコード共有の摩擦になっています。`ato` はこの層を取り除くために作りました。

---

### 仕組み

`ato run` はプロジェクトのファイルを直接読んで、必要なランタイムを推論します。

| プロジェクト | 推論されるランタイム |
|---|---|
| `pyproject.toml` / `uv.lock` / 単体 `.py` | `source/python` (uv バックエンド) |
| `package.json` + pnpm/npm/yarn | `source/node` |
| `deno.json` | `source/deno` |
| `Cargo.toml` | `source/native` |
| `*.wasm` | `wasm/wasmtime` |
| `capsule.toml` の `image =` フィールド | `oci/runc` |

Python と native ソースランタイムは [Nacelle](https://github.com/ato-run/nacelle) 経由で OS サンドボックス内で実行されます。macOS では `sandbox-exec`、Linux では `bwrap` を使います。

---

### capsule.toml：ゼロ設定から宣言設定へ

推論だけでは足りないときは `capsule.toml` を置きます。

```toml
schema_version = "0.3"
name           = "my-app"
version        = "0.1.0"
type           = "app"
run            = "python main.py"
runtime        = "source/python"
runtime_version = "3.12"

[network]
egress_allow = ["api.openai.com"]   # これ以外の外部通信はブロック

[requirements]
required_env = ["OPENAI_API_KEY"]   # 未設定なら実行前に警告 (v0.5.1 で abort 予定)
```

`capsule.toml` がないプロジェクトでもファイル構造から推論して動きます。

---

### ワークスペースを URL で共有する

```bash
ato encap
# → Share URL: https://ato.run/s/my-project@r1
```

生成された URL を渡すだけで、受け取った側は以下で再現できます。

```bash
ato decap https://ato.run/s/my-project@r1 --into ./copy
ato run ./copy
```

**シークレットはアップロードされません。** `ato encap` は「どの環境変数が必要か」という *契約* だけを記録し、値は記録しません。

---

### ato secrets：秘密情報の管理

```bash
ato secrets set OPENAI_API_KEY    # マスク入力
ato secrets list
ato run . --dry-run               # ハードコードされた秘密をスキャン
```

ファイルは `chmod 600` で保存されます。CI 環境を検出し、マスク入力はグレースフルにフォールバックします。

---

### バックグラウンド実行とサービス管理

```bash
ato run . --background
ato ps
ato logs --id <capsule-id> --follow
ato stop --id <capsule-id>
```

---

### 現状の制限事項

正直に書きます。v0.5 での既知の制限（詳細: [known-limitations.md](https://github.com/ato-run/ato-cli/blob/main/docs/known-limitations.md)）：

| ID | 内容 | 修正予定 |
|----|------|----------|
| L1 | `egress_allow` はソースランタイムで advisory（deny-all は有効） | v0.5.1 |
| L2 | `required_env` は警告のみで実行を止めない | v0.5.1 |
| L3 | `source/python` で `--sandbox` フラグ未対応 | v0.6 |
| L7 | `~/.ato/cache/synthetic/` が自動 GC されない | v0.5.1 |

---

### サンプルギャラリー

[ato-samples](https://github.com/ato-run/ato-samples) に 12 以上の実行可能サンプルがあります。

```bash
# BYOK AI チャット (OpenAI キーを使う)
ato run github.com/ato-run/ato-samples/02-apps/byok-ai-chat

# Wasm Hello World
ato run github.com/ato-run/ato-samples/01-capabilities/wasm-hello

# 「ato が今できないこと」のデモ (03-limitations/)
ato run github.com/ato-run/ato-samples/03-limitations/missing-env-preflight-failure
```

---

### インストール

```bash
# curl インストーラ
curl -fsSL https://ato.run/install.sh | sh

# Homebrew
brew tap ato-run/ato && brew install ato

# ソースから
cargo build -p ato-cli --release
```

---

### Capsule Protocol と Foundation Readiness

`capsule.toml` のスキーマは [UARC（Universal Application Runtime Contract）](https://github.com/ato-run/ato-cli/tree/main/conformance) という仕様として公開しています。目標は `ato` 以外の実装でも capsule を実行できるようにすることです。

現在の Foundation KPI は 0/6（v0.5 は基礎固めのリリースです）。  
コントリビューターとコンフォーマンステストのレビュアーを募集中です。

---

### リポジトリ

- CLI: https://github.com/ato-run/ato-cli
- サンプル: https://github.com/ato-run/ato-samples
- ドキュメント: https://docs.ato.run
- ライセンス: Apache 2.0

フィードバックや質問はコメントか [GitHub Issues](https://github.com/ato-run/ato-cli/issues) へお気軽にどうぞ。

---

*下書き作成日: 2026-04-23*
