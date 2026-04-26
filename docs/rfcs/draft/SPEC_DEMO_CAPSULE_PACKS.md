---
title: "Demo Capsule Packs Spec"
status: draft
date: 2026-04-17
author: "@Koh0920"
ssot:
  - "samples/"
  - "apps/ato-cli/src/cli/commands/run.rs"
  - "apps/ato-store/"
related:
  - "docs/researches/research_orchestrated_ato_demo_apps_20260415/"
---

# Demo Capsule Packs Spec

## 1. 概要

`ato run` のゼロコンフィグ体験をデモするための、厳選されたカプセルパックを整備する。
shadcn-admin, slash-admin 等の高品質Viteアプリを `ato run <github-url>` または
`ato run <store-id>` でワンコマンド起動できる状態にし、初期ユーザーの WOW 体験を確保する。

## 2. スコープ

### スコープ内

- Tier 1デモカプセルの選定・capsule.toml作成・動作検証
- ato store への登録（`ato publish`パイプライン）
- デスクトップランチャーへのピン留め表示
- `ato run github.com/owner/repo` のゼロコンフィグ推論改善

### スコープ外

- yarn/pnpmサポートの新規追加（別RFC）
- monorepoワークスペース検出（別RFC）
- ato storeのUI改善

## 3. 設計

### 3.1 デモカプセル選定基準

| 基準 | 要件 |
|------|------|
| ビルドツール | Vite（2秒以内起動）優先、Next.js許容 |
| パッケージマネージャ | npm（現在の最良サポート）|
| ライセンス | MIT / Apache-2.0（GPLv3不可）|
| ゼロコンフィグ | clone → npm install → npm run dev で起動 |
| ビジュアルインパクト | スクリーンショットで伝わる品質 |
| Stars | 500+ |

### 3.2 Tier 1 デモカプセル（5本）

| # | リポジトリ | Stars | カテゴリ | ポート | 起動コマンド |
|---|-----------|-------|---------|--------|-------------|
| 1 | satnaing/shadcn-admin | 11.7K | Dashboard | 5173 | `npm run dev` |
| 2 | d3george/slash-admin | 3K | Dashboard | 5173 | `pnpm dev` → npm要対応 |
| 3 | adrianhajdin/iphone | 1.6K | 3D/Creative | 5173 | `npm run dev` |
| 4 | Georgegriff/react-dnd-kit-tailwind-shadcn-ui | 771 | Kanban | 5173 | `npm run dev` |
| 5 | xyflow/react-flow-web-audio | 150 | Data Viz | 5173 | `npm run dev` |

### 3.3 capsule.toml テンプレート

```toml
schema_version = "1.0"
name = "shadcn-admin"
version = "0.1.0"
type = "app"

[metadata]
display_name = "Shadcn Admin Dashboard"
description = "Beautiful admin dashboard built with shadcn/ui"
category = "demo"
tags = ["dashboard", "react", "vite", "shadcn"]

[execution]
runtime = "source"
language = "node"
version = ">=18.0"
entrypoint = "npm"
command = "run dev"

[network]
egress_allow = []
```

### 3.4 Store 登録フロー

```
1. GitHub リポジトリをfork/clone
2. capsule.toml を追加
3. ato build でパッケージング
4. ato publish --registry ato.run で登録
5. ato run koh0920/shadcn-admin で起動確認
```

### 3.5 デスクトップランチャー統合

Launcherパネル（`ui/panels/launcher_v2.rs`）の「Pinned」セクションに
デモカプセルをハードコード表示：

```rust
fn demo_capsules() -> Vec<PinnedCapsule> {
    vec![
        PinnedCapsule { handle: "koh0920/shadcn-admin", label: "Dashboard" },
        PinnedCapsule { handle: "koh0920/iphone-3d", label: "3D iPhone" },
        // ...
    ]
}
```

### 3.6 ゼロコンフィグ推論の改善ポイント

現在の `source_inference` モジュールで改善が必要な項目：

| 項目 | 現状 | 改善 |
|------|------|------|
| Viteポート検出 | 手動 | `vite.config.{js,ts}` のserver.port解析 |
| dev script検出 | npm run dev固定 | package.json scripts.dev 解析 |
| 依存解決表示 | なし | `npm install` の進捗をxterm.jsに表示 |

## 4. インターフェース

### CLI

```bash
# Store経由
ato run koh0920/shadcn-admin

# GitHub直接
ato run github.com/satnaing/shadcn-admin

# ローカル（開発用）
ato run ./samples/shadcn-admin
```

### Desktop

- Launcherの「Pinned」セクションにデモカプセルカード表示
- クリックで `navigate_to_url("koh0920/shadcn-admin")` 実行
- capsule://koh0920/shadcn-admin ディープリンク対応

## 5. セキュリティ

- デモカプセルはサンドボックス内で実行（egress deny-by-default）
- 信頼レベルは "untrusted"（ユーザーが明示的にcapability付与しない限り制限）
- ネットワークアクセスが必要なカプセルは capsule.toml の egress_allow で明示

## 6. 既知の制限

- pnpm必須のリポジトリ（slash-admin）は現状npm fallbackが必要
- Node.js 18+ がホストに必要（toolchain自動解決は将来対応）
- 初回起動時の npm install に時間がかかる（キャッシュは2回目以降有効）

## 実装計画

### Phase 1: カプセル作成（1-2日）
1. shadcn-admin の capsule.toml 作成・ローカル動作確認
2. iphone (Three.js) の capsule.toml 作成・動作確認
3. react-dnd-kit Kanban の capsule.toml 作成・動作確認

### Phase 2: Store 登録（1日）
4. 3本を ato publish で store に登録
5. `ato run koh0920/<name>` で起動確認

### Phase 3: Desktop 統合（1日）
6. Launcher パネルにピン留め表示
7. デモカプセルカードのUI実装

### Phase 4: 推論改善（2-3日）
8. Vite ポート自動検出
9. package.json scripts.dev 解析
10. 依存解決の進捗表示

## 参照

- `samples/react-vite/capsule.toml` — 既存サンプルの参考
- `apps/ato-cli/src/application/source_inference/mod.rs` — ソース推論エンジン
- `docs/researches/research_orchestrated_ato_demo_apps_20260415/` — デモアプリ調査
