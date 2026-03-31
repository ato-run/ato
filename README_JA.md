# Ato: The Agentic Meta-Runtime 🚀

[![CI](https://github.com/ato-run/ato-cli/actions/workflows/build-multi-os.yml/badge.svg?branch=dev)](https://github.com/ato-run/ato-cli/actions/workflows/build-multi-os.yml)
[![GitHub Release](https://img.shields.io/github/v/release/ato-run/ato-cli)](https://github.com/ato-run/ato-cli/releases)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange?logo=rust)](https://www.rust-lang.org/)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-6f42c1)
[![Issues](https://img.shields.io/github/issues/ato-run/ato-cli)](https://github.com/ato-run/ato-cli/issues)
[![Last Commit](https://img.shields.io/github/last-commit/ato-run/ato-cli/dev)](https://github.com/ato-run/ato-cli/commits/dev)

日本語 | [English](README.md)

> **AI時代のNix代替。URLを渡すだけで、1秒でセキュアな実行環境を自律的に構築する次世代メタランタイム。**

Ato（アト）は、Dockerの「重いビルド待ち」と、Nixの「難解な設定言語」を過去のものにします。
ソースコードから必要なランタイムと依存関係を**自律的（Agentic）に割り出し、Zero-Trust サンドボックス（[Nacelle](https://github.com/ato-run/nacelle)）内で一瞬にして**安全に具現化・実行します。

---

## ✨ Atoの「3つの魔法」

### 1. 設置もクローンも不要。URLから即座に「隔離実行」 (`ato run`)

`ato run` は、GitHubリポジトリ、レジストリ、あるいは単一のスクリプトを、ローカル環境を汚さず即座に実行します。実行に必要な依存関係は一時的な領域で解決される**Ephemeral（使い捨て）な実行パス**です。

```bash
# GitHubリポジトリを直接実行。ソース取得、推論、隔離実行をこれ1発で。
ato run github.com/user/my-app

# レジストリからパッケージをインストールなしで即時実行。
ato run publisher/awesome-tool

# 単一のスクリプトを、設定ゼロで安全に実行。
ato run scrape.py
```

### 2. 他人のリポジトリから、完璧な開発環境を1秒で錬成 (`ato init`)

`git clone` すら不要です。URLを渡すだけでソースを取得・解析し、LSPやツールチェーンまで含めた「完全な再現性」を持つ開発用ワークスペースを物理ディレクトリに**具現化（Materialize）**します。

```bash
# クローンから環境構築、開発シェルの準備まで完結。
ato init github.com/user/repo my-project
```

### 3. アプリをセキュアに公開し、デスクトップへ統合 (`ato publish / install`)

開発したツールは `ato publish` でレジストリへ登録。利用者は `ato install` を叩くだけで、OSネイティブなデスクトップアプリやCLIとして**プロジェクション（投影）**され、日常的に利用可能になります。

```bash
# 自分のツールを不変なアーティファクトとしてレジストリへ登録
ato publish

# パッケージをローカルに固定し、デスクトップアプリとしてOSに登録
ato install publisher/awesome-app
```

---

## 🛠 サポートツール・言語の範囲

Ato の推論エンジンは、以下の言語やプロジェクト構造を自律的に検出し、最適なランタイム環境を構築します。
これらがない場合でも、`ato run` は汎用的なプロセスエグゼキューターとして動作し、`ato.lock.json` を通じてあらゆるバイナリの再現性を担保します。

- **単一ファイル実行 (Single-file Scripts)**
  - **Python (`.py`)**: PEP 723 形式のインラインメタデータをパースし、ライブラリを自動解決。
  - **TypeScript / TSX (`.ts`, `.tsx`)**: Deno ベースで依存関係を自動推論。
- **プログラミング言語とランタイム**
  - **TypeScript / JavaScript**:
    - **Deno (推奨)**: `deno.json` 標準実行。URL インポートや `npm:` プロトコル対応。
    - **Node.js**: `package.json` や各種ロックファイルを検出。互換モード対応。
  - **Python**: `uv` を標準採用。`pyproject.toml` 等を推論し、隔離された仮想環境を構築。
  - **Rust / Go**: `Cargo.toml` や `go.mod` を検出。
  - **WebAssembly / OCI**: `.wasm` バイナリの直接実行や、既存の Docker イメージのサンドボックス実行。
- **デスクトップ / Web フレームワーク**
  - **Tauri / Electron / Wails**: デスクトップアプリのネイティブ統合（プロジェクション）。
  - **Static Web**: `index.html` 中心サイトの内蔵Webサーバープレビュー。

---

## 🚀 クイックスタート

ローカルソース開発時のワークフローです。

```bash
# ビルド
cargo build -p ato-cli

# nacelle エンジンを未導入の場合（推奨）
./target/debug/ato config engine install --engine nacelle

# 互換: setup サブコマンド
./target/debug/ato setup --engine nacelle

# ローカルディレクトリを実行
./target/debug/ato run .

# 開発時ホットリロード
./target/debug/ato run . --watch

# バックグラウンド管理
./target/debug/ato run . --background
./target/debug/ato ps
./target/debug/ato logs --id <capsule-id> --follow
./target/debug/ato close --id <capsule-id>
```

---

## 📖 主要コマンドリファレンス

```bash
ato run [path|publisher/slug|github.com/owner/repo] [--registry <url>]
ato init [path] [--yes]
ato install <publisher/slug> [--registry <url>]
ato install --from-gh-repo <github.com/owner/repo>
ato build [dir] [--strict-v3] [--force-large-payload]
ato publish [--registry <url>] [--artifact <file.capsule>] [--scoped-id <publisher/slug>]
ato ps
ato close --id <capsule-id> | --name <name> [--all] [--force]
ato logs --id <capsule-id> [--follow]
ato inspect lock [path] [--json]
ato inspect preview [path] [--json]
ato inspect diagnostics [path] [--json]
ato inspect requirements <path|publisher/slug> [--json]
ato source sync-status --source-id <id> --sync-run-id <id>
ato source rebuild --source-id <id> [--ref <branch|tag|sha>] [--wait]
ato search [query]
ato registry serve --host 127.0.0.1 --port 18787 [--auth-token <token>]
```

> 💡 `ato inspect` ファミリーには、設定プレビュー（`preview`）、問題診断（`diagnostics`）、修正提案（`remediation`）、要件JSONの出力（`requirements`）など、デバッグや開発支援に便利なコマンドが揃っています。

---

## 🏗 アーキテクチャと主要機能

### Lock-native 入力モデル

人間用の曖昧な設定ではなく、機械可読な `ato.lock.json` が設定の「唯一の正本（SSOT）」です。このファイルがあれば、マシンの差異を問わず全く同じ環境の復元が保証されます。（まだ移行前なら旧形式の設定も使え、`ato init` で移行可能です）。

### 統合された配信パイプライン

消費者（`run`/`install`）と生産者（`build`/`publish`）のフローを統合しています。`ato build` で巨大なイメージを落とす代わりに、必要なバイナリだけを CAS (Content-Addressable Storage) からハードリンク展開するため、オーバーヘッドは実質ゼロです。

### Native Delivery（デスクトップアプリの投影）

macOS等のネイティブアプリの配信・統合機能です。TauriやElectronなどのプロジェクト構造からエントリポイント（`.app` など）を自動推論し、ネイティブ配信に必要な設定をすべて `ato.lock.json` へ自律的に解決・記録します（手動でのマニフェスト設定ファイルの記述は不要です）。
メタデータを含んだ成果物を利用者が `ato install` すると、OSのアプリケーション領域へシンボリックリンク等で**投影（プロジェクション）**され、通常のデスクトップアプリとして起動可能になります。

### 動的アプリのカプセル化 (Web + Services Supervisor)

API、Dashboard、Workerなど複数のサービスを1つのカプセルにまとめられます。`[services]` を定義すると、`ato run` がDAG順（依存関係順）にサービスを起動し、readiness probe で待機、ログのプレフィックス付与、そしていずれかの終了時に一括停止するようオーケストレーションします。

### レジストリと公開モデル

`ato publish` はどこに公開するかで動作が変わります。公開は最大6つのステージ（Prepare → Build → Verify → Install → Dry-run → Publish）を順に実行します。

1. **Personal Dock（マイ・ドックへの公開）**
   `ato login` 済みなら `--registry` 指定なしで、Personal Dock へ自動アップロードされます（実行: Prepare 〜 Publish）。
2. **Custom / Private レジストリへの公開**
   `--registry <url>` を指定して独自の社内ストア等にアップロード可能です。ローカルのHTTPレジストリ（`ato registry serve`）へのパブリッシュ等、開発中のE2Eにも最適です。
3. **公式 Store への公開 (`https://api.ato.run`)**
   セキュアなパイプラインを確保するため、公式 Store は常に CI 経由による OIDC 認証で公開します。ローカルでの直接アップロードは行わず、手元での実行は Publish直前(handoff) の診断情報の送信で止まります。`ato gen-ci` で連携用のGitHub Actionsを生成できます。

---

## 🛡 セキュリティと実行ポリシー (Zero-Trust)

Ato はデフォルトで厳格（Fail-closed）に動作し、予期せぬ実行からシステムを保護します。

- **プロセスの隔離**: デスクトップにプロジェクションされたアプリやローカルソースであっても、Atoは対象を独自の軽量サンドボックス（**[Nacelle](https://github.com/ato-run/nacelle)**）内で起動します。
- **ファイルシステム保護**: デフォルトで Read-Only に制限され、AI生成コードや未知のライブラリも安全な範囲でのみ試すことができます。
- **ネットワーク制御**: 実行時に許可されたドメイン以外への通信は遮断されます（Fail-Closed）。
- **環境変数の厳密な管理**: マニフェストの `required_env` に列挙された必須変数が未設定だと、不足していることを警告して起動前に即座に停止します。

---

## 📦 ランタイム隔離ポリシー (Tiers)

ランタイムの種類によって、必要な隔離レベル（Tier）が異なります。NodeやDenoは Tier 1 としてそのまま動作しますが、Pythonやネイティブ環境は安全性を担保するために強力なサンドボックスが必要です。

| ランタイム      | Tier  | 必要な構成                                                      |
| --------------- | ----- | --------------------------------------------------------------- |
| `web/static`    | Tier1 | `driver = "static"` + 空きポート指定 (ロックファイル不要)       |
| `web/deno`      | Tier1 | `ato.lock.json` または `deno.lock` / `package-lock.json`        |
| `web/node`      | Tier1 | `ato.lock.json` または `package-lock.json` (Deno互換で自動実行) |
| `web/python`    | Tier2 | `uv.lock` + サンドボックス起動推奨                              |
| `source/deno`   | Tier1 | `ato.lock.json` または `deno.lock` / `package-lock.json`        |
| `source/node`   | Tier1 | `ato.lock.json` または `package-lock.json` (Deno互換で自動実行) |
| `source/python` | Tier2 | `uv.lock` + サンドボックス起動推奨                              |
| `source/native` | Tier2 | (コンパイル済みの実行バイナリ)                                  |

**Tier 1** は特別なフラグなしで動作します。Node も Tier 1 のため、実行に際して `--unsafe` のような権限迂回フラグは不要です。

**Tier 2** (`source/native`, `source/python`, `web/python`) を動かすには、先述の [Nacelle](https://github.com/ato-run/nacelle) エンジンが必要です（未インストールの場合はフェイルクローズで停止します）。以下のいずれかで準備してください。

```bash
ato config engine install --engine nacelle # エンジンを自動インストール
ato run --nacelle <path>                   # 実行時にパスを手動で渡す
# または環境変数 NACELLE_PATH を設定する
```

---

## ⚙️ 環境変数リファレンスと認証

CLIの挙動やデフォルトの接続先は、以下の環境変数で制御されます。

| 変数                        | 説明                                                     | デフォルト            |
| --------------------------- | -------------------------------------------------------- | --------------------- |
| `CAPSULE_WATCH_DEBOUNCE_MS` | `run --watch` のデバウンス間隔（ミリ秒）                 | `300`                 |
| `CAPSULE_ALLOW_UNSAFE`      | `1` に設定すると `--dangerously-skip-permissions` を許可 | —                     |
| `ATO_TOKEN`                 | ローカル・私設レジストリ向け公開やCIで使う認証トークン   | —                     |
| `ATO_STORE_API_URL`         | `ato search` や `install` で使用する解決先API            | `https://api.ato.run` |
| `ATO_STORE_SITE_URL`        | ストアWebのベースURL                                     | `https://ato.run`     |

### 認証の仕組み (`ato login`)

デフォルトの認証情報は `${XDG_CONFIG_HOME:-~/.config}/ato/credentials.toml` に保存されます。`ato` が認証を解決する優先順位は以下の通りです。

1. `ATO_TOKEN` 環境変数
2. OSのセキュアキーリング
3. `~/.config/ato/credentials.toml`
4. 旧形式の `~/.ato/credentials.json` (フォールバック用)

---

## 🤝 コントリビュート

コントリビュートに関心をお持ちいただきありがとうございます。開発の詳細やアーキテクチャの内部ロジックについてはコアのドキュメントを参照してください。

- バグ報告や機能要望は GitHub Issues にて受け付けています。
- 議論や質問については Discord コミュニティへご参加ください。

## License

Apache License 2.0 (SPDX: Apache-2.0). See [LICENSE](LICENSE).
