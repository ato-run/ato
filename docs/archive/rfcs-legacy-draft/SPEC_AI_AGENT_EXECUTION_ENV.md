---
title: "AI Agent Execution Environment Spec"
status: draft
date: 2026-04-17
author: "@Koh0920"
ssot:
  - "apps/ato-desktop/src/automation/"
  - "apps/ato-cli/src/cli/commands/run.rs"
  - "apps/ato-desktop/src/orchestrator.rs"
related:
  - "docs/researches/devpulse_ato_ecosystem_20260415/"
---

# AI Agent Execution Environment Spec

## 1. 概要

ato のカプセルシステムを AI エージェントの安全な実行環境として位置付ける。
OpenClaw のゼロコンフィグ起動体験をベンチマークとし、
`ato run <github-url>` でAIエージェントアプリを即座に起動できる状態を目指す。

ato の差別化:
- **ローカルファースト**（E2B/Daytona は全てクラウド）
- **サンドボックス標準装備**（OpenClaw はオプション）
- **シークレット安全注入**（.env 漏洩ゼロ）
- **egress 制御**（AIエージェントのネットワーク活動を制限）

## 2. スコープ

### スコープ内

- OpenClaw のゼロコンフィグ起動（`ato run github.com/openclaw/openclaw`）
- AI エージェントアプリの典型パターン分析と推論ルール追加
- API Key の安全な注入（Secret Auto-Injection Spec と連携）
- エージェント実行時の egress 制御テンプレート
- Desktop UI でのエージェント状態可視化

### スコープ外

- OpenClaw の内部アーキテクチャ変更
- 独自の AI エージェントフレームワーク構築
- クラウドサンドボックス（E2B互換）の提供
- LLM プロバイダーとの直接統合

## 3. 設計

### 3.1 OpenClaw ゼロコンフィグ起動の分析

OpenClaw のセットアップ要件:
```
1. Python 3.12+
2. Docker（オプション、サンドボックス用）
3. uv（Python パッケージマネージャ）
4. 環境変数: ANTHROPIC_API_KEY 等
5. git clone → uv run openclaw
```

ato でのゼロコンフィグ目標:
```bash
# ユーザーが実行するのはこれだけ
ato run github.com/openclaw/openclaw

# ato が自動で:
# 1. リポジトリを clone
# 2. Python 3.12 を toolchain から解決
# 3. uv を toolchain から解決
# 4. 依存関係をインストール
# 5. シークレット注入（ANTHROPIC_API_KEY等）
# 6. サンドボックス内で起動
# 7. ポート検出してブラウザ/desktop で開く
```

### 3.2 AI エージェントアプリのパターン分類

| パターン | 例 | 検出方法 | 特徴 |
|---------|---|---------|------|
| Python + LLM SDK | OpenClaw, LangChain apps | `anthropic`/`openai` in requirements.txt | API Key 必須 |
| Node + LLM SDK | Vercel AI SDK apps | `@anthropic-ai/sdk` in package.json | API Key 必須 |
| Python + Web UI | Streamlit/Gradio apps | `streamlit`/`gradio` in requirements.txt | ポート自動割当 |
| Docker Compose | Multi-service agents | `docker-compose.yml` 存在 | コンテナ管理 |
| Static + API | フロントエンドのみ | `fetch` to LLM API in source | ブラウザ直接 |

### 3.3 Source Inference 拡張

`source_inference/mod.rs` に AI エージェント検出ルールを追加:

```rust
/// AI エージェントアプリの検出
fn detect_ai_agent_pattern(workspace: &Workspace) -> Option<AiAgentHint> {
    // Python パターン
    if let Some(reqs) = workspace.read_file("requirements.txt") {
        let deps = reqs.to_lowercase();
        if deps.contains("anthropic") || deps.contains("openai")
            || deps.contains("langchain") || deps.contains("openclaw") {
            return Some(AiAgentHint {
                runtime: "python",
                required_secrets: detect_required_api_keys(&deps),
                egress_hints: detect_egress_patterns(&deps),
            });
        }
    }

    // pyproject.toml (uv/poetry) パターン
    if let Some(pyproject) = workspace.read_file("pyproject.toml") {
        if pyproject.contains("anthropic") || pyproject.contains("openai") {
            return Some(AiAgentHint {
                runtime: "python",
                required_secrets: detect_required_api_keys_toml(&pyproject),
                egress_hints: vec!["api.anthropic.com", "api.openai.com"],
            });
        }
    }

    // Node パターン
    if let Some(pkg) = workspace.read_file("package.json") {
        if pkg.contains("@anthropic-ai/sdk") || pkg.contains("openai") {
            return Some(AiAgentHint {
                runtime: "node",
                required_secrets: detect_required_api_keys_node(&pkg),
                egress_hints: vec!["api.anthropic.com", "api.openai.com"],
            });
        }
    }

    None
}
```

### 3.4 API Key 要求の自動検出と注入フロー

```
ato run github.com/openclaw/openclaw
│
├─ 1. Clone & Inference
│  ├─ detect_ai_agent_pattern() → AiAgentHint
│  └─ required_secrets: ["ANTHROPIC_API_KEY"]
│
├─ 2. Secret Resolution
│  ├─ Desktop: SecretStore.get("ANTHROPIC_API_KEY")
│  ├─ CLI: ato secrets get ANTHROPIC_API_KEY (Keychain)
│  └─ Fallback: --prompt-env で対話的に入力
│     └─ "This capsule requires ANTHROPIC_API_KEY. Enter value:"
│     └─ 入力値を ato secrets set で保存（次回以降自動注入）
│
├─ 3. Egress Policy 自動構成
│  ├─ egress_hints → egress_allow に変換
│  │   ["api.anthropic.com", "api.openai.com"]
│  └─ ユーザーに確認表示:
│     "Allow network access to api.anthropic.com? [Y/n]"
│
├─ 4. Sandbox Execution
│  ├─ Python toolchain 自動解決
│  ├─ uv/pip 依存インストール
│  ├─ ATO_SECRET_ANTHROPIC_API_KEY=sk-... 注入
│  └─ サンドボックス内で起動
│
└─ 5. UI 表示
   ├─ Desktop: ターミナル or WebView（ポートがあれば）
   └─ CLI: stdout/stderr 直接表示
```

### 3.5 Egress テンプレート

AI エージェントアプリ向けのプリセット egress ポリシー:

```toml
# capsule.toml での egress 設定例
[network]
egress_allow = [
    "api.anthropic.com",
    "api.openai.com",
    "generativelanguage.googleapis.com",
    "api.mistral.ai",
    "api.groq.com",
]
```

自動検出時のデフォルト:
| SDK 検出 | 自動許可ホスト |
|---------|---------------|
| `anthropic` | `api.anthropic.com` |
| `openai` | `api.openai.com` |
| `google-generativeai` | `generativelanguage.googleapis.com` |
| `mistralai` | `api.mistral.ai` |
| `groq` | `api.groq.com` |

### 3.6 Desktop エージェント表示

Desktop でAIエージェントカプセルを開いた場合の表示:

```
┌─────────────────────────────────────────────┐
│  ◆ OpenClaw Agent          [Reload] [Stop]  │
├─────────────────────────────────────────────┤
│  Status: Running                            │
│  Runtime: Python 3.12 (sandbox)             │
│  Egress: api.anthropic.com (2 more)         │
│  Secrets: ANTHROPIC_API_KEY ✓               │
│                                             │
│  ┌─ Terminal ─────────────────────────────┐ │
│  │ $ uv run openclaw                      │ │
│  │ OpenClaw v0.31.0 starting...           │ │
│  │ Gateway listening on :18789            │ │
│  │ Web UI: http://localhost:18789         │ │
│  └────────────────────────────────────────┘ │
│                                             │
│  [Open Web UI]  [View Logs]  [Settings]     │
└─────────────────────────────────────────────┘
```

### 3.7 OpenClaw 固有の対応事項

| 項目 | 現状 | 必要な対応 |
|------|------|-----------|
| Python 3.12+ | toolchain に3.12.7あり | バージョン制約 `>=3.12` の解決 |
| uv パッケージマネージャ | toolchain にuvあり | `uv run` エントリポイント対応 |
| Docker（オプション） | 非対応 | スキップ（sandbox で代替） |
| WebSocket (:18789) | egress 制御外 | localhost は常時許可済み |
| 設定ファイル生成 | init wizard | `--yes` フラグで非対話 or デフォルト |
| 複数 LLM プロバイダー | API Key 複数 | Secret 複数注入対応済み |

### 3.8 `uv run` エントリポイント対応

OpenClaw は `uv run openclaw` で起動する。source_inference に追加:

```rust
// pyproject.toml に [project.scripts] がある場合
// → uv run <script-name> をエントリポイントとして推論
fn detect_uv_entrypoint(pyproject: &str) -> Option<String> {
    // [project.scripts]
    // openclaw = "openclaw.__main__:main"
    // → entrypoint = "uv", command = "run openclaw"
}
```

## 4. インターフェース

### CLI

```bash
# ゼロコンフィグ起動
ato run github.com/openclaw/openclaw

# API Key を事前登録
ato secrets set ANTHROPIC_API_KEY sk-ant-...

# API Key + 起動（初回のみプロンプト）
ato run github.com/openclaw/openclaw --prompt-env

# egress を明示的に許可
ato run github.com/openclaw/openclaw \
  --allow api.anthropic.com \
  --allow api.openai.com
```

### Desktop

```
Omnibar: github.com/openclaw/openclaw [Enter]
→ 推論 → "AI Agent detected. Required: ANTHROPIC_API_KEY"
→ Secret 注入 → サンドボックス起動 → ターミナル表示
→ ポート検出 → "Open Web UI" ボタン表示
```

### capsule.toml（AI エージェント向け）

```toml
schema_version = "1.0"
name = "openclaw"
type = "app"

[execution]
runtime = "source"
language = "python"
version = ">=3.12"
entrypoint = "uv"
command = "run openclaw"

[network]
egress_allow = [
    "api.anthropic.com",
    "api.openai.com",
]

[metadata]
category = "ai-agent"
tags = ["ai", "agent", "assistant", "openclaw"]
required_env = ["ANTHROPIC_API_KEY"]
```

## 5. セキュリティ

- API Key は環境変数としてのみ注入（ファイルに書かない）
- egress はデフォルト deny、検出されたAPIホストのみ許可提案
- サンドボックス内実行（nacelle）でファイルシステムアクセス制限
- AIエージェントが生成するコードもサンドボックス内で実行
- プロンプトインジェクション対策は AI エージェント側の責務（ato は実行環境のみ）

## 6. 既知の制限

- Docker Compose ベースのエージェントは非対応（カプセルモデルと相容れない）
- GPU アクセスは nacelle sandbox を経由しない場合のみ利用可能
- ローカル LLM（Ollama）との連携は localhost egress で対応可能だが自動検出なし
- OpenClaw の init wizard は対話的入力が必要 → `--yes` デフォルト対応が必要

## 実装計画

### Phase 1: OpenClaw ゼロコンフィグ起動（3-4日）

1. **source_inference** — `uv run` エントリポイント検出
   - pyproject.toml の `[project.scripts]` 解析
   - `uv` を toolchain から解決

2. **source_inference** — AI エージェントパターン検出
   - requirements.txt / pyproject.toml からLLM SDK検出
   - required_env の自動推論

3. **run コマンド** — `--prompt-env` のシークレット保存統合
   - 入力値を `ato secrets set` に保存
   - 次回起動時に自動注入

4. **動作確認** — `ato run github.com/openclaw/openclaw` E2Eテスト

### Phase 2: Egress 自動構成（1-2日）

5. **egress_policy** — SDK 検出 → egress_allow 自動提案
   - `anthropic` → `api.anthropic.com` マッピング
   - ユーザー確認プロンプト

6. **CLI/Desktop** — egress 許可のUX改善
   - "Allow api.anthropic.com? [Y/n]" プロンプト
   - Desktop: permission prompt UI 活用

### Phase 3: Desktop 統合（2-3日）

7. **Inspector パネル** — AI エージェント情報表示
   - Runtime, Egress, Secrets ステータス
   - "Open Web UI" ボタン（ポート検出時）

8. **Launcher** — AI Agent カテゴリ追加
   - カテゴリフィルタ `ai-agent`
   - 人気エージェントの推奨表示

### Phase 4: エコシステム拡張（将来）

9. 他の AI エージェントフレームワーク対応
   - LangChain / LangGraph アプリ
   - CrewAI
   - AutoGen

10. エージェント実行ログの構造化
    - tool call ログ
    - API 使用量トラッキング

## 参照

- `apps/ato-cli/src/application/source_inference/mod.rs` — ソース推論エンジン
- `apps/ato-desktop/src/automation/` — AutomationHost（エージェント連携基盤）
- `apps/ato-desktop/src/orchestrator.rs:2195-2800` — ato-run REPL（子プロセス管理）
- `apps/ato-cli/src/cli/commands/run.rs` — run コマンド実装
- OpenClaw ドキュメント: https://docs.openclaw.ai/
- DevPulse調査: `docs/researches/devpulse_ato_ecosystem_20260415/`
