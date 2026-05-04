# RFCs — 技術仕様ドキュメント

すべての技術仕様は RFC として管理する。

```
rfcs/
├── accepted/      ← 確定仕様（現行実装の根拠）
├── draft/         ← 議論中・未確定（実装前または変更検討中）
├── archived/      ← 廃止・旧バージョン・非仕様ドキュメント
├── TEMPLATE_ADR.md
├── TEMPLATE_SPEC.md
└── README.md      ← このファイル
```

## ドキュメント種別

| 種別 | テンプレート | 用途 | 目安 |
|------|-------------|------|------|
| **ADR** | `TEMPLATE_ADR.md` | 設計上の意思決定を記録 | 1-2 ページ。「なぜこう決めたか」 |
| **SPEC** | `TEMPLATE_SPEC.md` | コンポーネント/機能の仕様 | 数ページ〜。「どう動くか」 |

## ライフサイクル

```
draft/ で作成・議論  →  確定  →  accepted/ に移動
                    →  破棄  →  archived/ に移動
accepted/ の仕様が陳腐化  →  archived/ に移動
```

## ステータス

| ステータス | 場所 | 意味 |
|-----------|------|------|
| `draft` | `draft/` | 設計中・議論中。実装の根拠として使わない |
| `accepted` | `accepted/` | 確定済み。現行実装と一致している |
| `archived` | `archived/` | 廃止・後継あり・旧バージョン |

## フォーマットルール

### 1. YAML frontmatter（必須）

すべての RFC は先頭に frontmatter を持つ:

```yaml
---
title: "ドキュメントタイトル"
status: draft          # draft | accepted | archived
date: YYYY-MM-DD
author: "@github_handle"
ssot:                  # SPEC のみ。Source of Truth のコードパス
  - "apps/xxx/src/yyy.rs"
related:               # 関連ドキュメント（任意）
  - "CAPSULE_CORE.md"
---
```

### 2. ファイル名

| 種別 | 形式 | 例 |
|------|------|-----|
| ADR | `ADR-NNN-kebab-case-title.md` | `ADR-001-runtime-selection-order.md` |
| SPEC | `SCREAMING_SNAKE_SPEC.md` | `CAPSULE_CORE.md`, `NACELLE_SPEC.md` |

- ADR は通し番号 (`NNN`) で管理
- SPEC はコンポーネント名ベース

### 3. セクション構成

**ADR** — Context → Decision → Alternatives Considered → Consequences

**SPEC** — 概要 → スコープ → 設計 → インターフェース → セキュリティ → 既知の制限 → 参照

不要なセクションは省略可。セクションは番号付き (`## 1. 概要`) を推奨。

### 4. Source of Truth (SSOT)

コードが正。仕様ドキュメントはコードの説明であり、乖離が見つかったらコードを信じる。
SPEC には `ssot` フィールドでコードパスを明記し、本文中でも `file:line` 形式で参照する。

### 5. 言語

日本語を基本とする。セクション見出しはどちらでもよいが、1ドキュメント内で統一する。

## 新しい RFC を追加するとき

1. テンプレートをコピーして `draft/` に配置
2. frontmatter を埋める
3. PR でレビュー
4. 確定したら `accepted/` に移動し、`status: accepted` に更新

## 公開サイト

- GitHub Pages: <https://ato-run.github.io/ato/>
- 公開ナビゲーションには `accepted/` と `draft/` のみを含める。`archived/` はリポジトリ内に保持する。

ローカルで確認するとき:

```bash
cd docs/rfcs
python3 -m http.server 4173
```

ブラウザで <http://localhost:4173> を開く。
