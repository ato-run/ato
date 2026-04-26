# 📄 Capsule Naming & Identity Specification (Draft)

**Document ID:** `NAMING_SPEC`
**Status:** Draft v1.0
**Target:** ato-store / ato-desktop

## 1. 識別子フォーマット (Identifier Format)

すべてのカプセルは、**Scoped ID**（パブリッシャーID + スラッグ）によって一意に識別される。

### 1.1 形式

```
{publisher_handle}/{capsule_slug}

```

- **例:** `ato-core/byok-ai-chat`
- **区切り文字:** `/` (スラッシュ)

### 1.2 制約 (Validation)

- **文字種:** 英小文字 (`a-z`)、数字 (`0-9`)、ハイフン (`-`) のみ。
- **長さ:** 各セグメント 3文字以上、32文字以下。
- **正規表現:** `^[a-z0-9][a-z0-9-]{1,30}[a-z0-9]$`
- **禁止事項:** 連続するハイフン、先頭・末尾のハイフンは不可。

## 2. パブリッシャーID (Publisher Identity)

### 2.1 取得ルール (Acquisition)

- **先着順 (First-come, First-served):**
- ユーザーは、未取得の任意の `publisher_handle` を登録できる。
- 技術的な本人確認（メール認証、OAuth、DNS認証）は**行わない**。

- **アカウント制約:**
- Ato Desktop クライアントは、同一セッションで単一の Ato アカウントのみログイン可能。
- これにより、一人のユーザーが大量の捨てアカウントを作成・管理するインセンティブを構造的に抑制する。

### 2.2 予約語 (Reserved Handles)

システムの安全性と公式機能を保護するため、以下のIDは登録不可とする。

- `ato`, `admin`, `root`, `system`, `store`, `official`, `support`, `security` 等。

## 3. なりすまし・商標保護 (Brand Protection Policy)

技術的なガードではなく、**「サービス運用ポリシー（利用規約）」** として紛争を解決する。

### 3.1 紛争解決プロセス (Dispute Resolution)

- **原則:** パブリッシャーIDは登録者本人のものとする。
- **例外 (商標侵害):**
- 正当な商標権者からの申し立てがあった場合（例: 第三者が `google` や `openai` を取得）、運営は調査の上、該当IDを剥奪または権利者へ譲渡する権利を持つ。
- 悪意あるなりすまし（Typosquatting等でユーザーを害する場合）は、運営判断でアカウント停止（BAN）とする。

### 3.2 表示名 (Display Name)

- IDとは別に、自由な文字列の「表示名」を設定可能とする。
- **例:**
- ID: `ato-core`
- Display Name: `Ato Official`

## 4. メタデータ連携 (Integration)

`.sync` データおよび `capsule.toml` におけるIDの扱い。

### 4.1 `capsule.toml`

```toml
# 必須フィールド
name = "ato-core/byok-ai-chat"  # Scoped ID

# 任意フィールド (UI表示用)
display_name = "BYOK AI Chat"
description = "Secure AI chat client for local LLMs."

```

### 4.2 `.sync` (Manifest)

データを作成したアプリのトレーサビリティを確保するため、Scoped ID を記録する。

```rust
// manifest.toml (inside .sync)
[meta]
created_by = "ato-core/byok-ai-chat"  # 以前の "ato-desktop" から修正
schema_version = "1.0"

```
