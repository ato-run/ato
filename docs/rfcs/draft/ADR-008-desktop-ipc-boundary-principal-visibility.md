---
title: "ADR-008: Desktop Host Bridge IPC 境界設計 — Principal 解決・Visibility 3-tier・Deny-by-default"
status: draft
date: 2025-08-01
author: "@Koh0920"
related:
  - "draft/CAPSULE_IPC_SPEC.md"
  - "CAPSULE_CORE.md"
---

# ADR-008: Desktop Host Bridge IPC 境界設計

## Context

`ato-desktop` の WebView ↔ ホスト間 IPC (`system_capsule/ipc.rs`) は長い間 fire-and-forget 設計で、以下の問題を抱えていた。

1. **JS 側の `capsule` フィールドを信用していた** — WebView 内の JS が任意の文字列を `capsule` に渡すことができ、意図しないハンドラへのディスパッチが可能だった（spoof 攻撃）。
2. **Typed error が JS へ返せなかった** — エラーは `warn & drop` のみ。JS 側は成功/失敗を判断できず、Promise ベースの API が作れなかった。
3. **System capsule と guest capsule の IPC surface が混在** — system-only コマンドが任意の WebView から呼べる状態だった。
4. **IPC コードが `system_capsule/` 配下のみに置かれていた** — 再利用・テストが困難だった。

これらの問題は security boundary の曖昧さであり、ato-desktop の信頼性・監査可能性に影響する。

## Decision

以下の設計原則を採用し、`feat/ipc-boundary-redesign` ブランチ（9フェーズ）で実装した。

### 1. Principal を Rust 側で解決する（JS の `capsule` フィールドを無視）

```rust
pub enum IpcPrincipal {
    SystemCapsule { id: SystemCapsuleId, canonical_slug: String, materialized_root: PathBuf },
    Capsule { handle: String, target: String, execution_id: String, session_id: String, origin: String },
    DesktopShell,
}
```

- **System capsule**: GPUI ホストウィンドウハンドル → `SystemCapsuleWindowRegistry` → `IpcPrincipal::SystemCapsule`
- **Guest capsule**: pane/session → `GuestSessionContext` → `IpcPrincipal::Capsule`
- JS が送る `capsule` / `isSystem` フィールドは principal 解決に使用しない。スプーフィング不可能。

### 2. Visibility 3-tier (deny-by-default registry)

```rust
pub enum IpcVisibility {
    PublicCapsule,   // 通常 guest capsule から呼べる（capsule.*, shell.openExternal 等）
    SystemCapsule,   // system capsule 専用（session.*, settings.*, registry.* 等）
    InternalOnly,    // WebView transport から到達不能（debug.* 等、test harness 専用）
}
```

- `IpcCommandRegistry` に登録されていないコマンドはすべて `UnknownCommand` エラーを返す（drop しない）。
- `InternalOnly` コマンドは WebView transport adapter レイヤーで reject（registry lookup より前）。
- visibility が `SystemCapsule` のコマンドを `PublicCapsule` の principal から呼ぶと `Forbidden` エラーを返す。

### 3. Host window binding を前提とする

- 各 system capsule ウィンドウは開かれた瞬間に `SystemCapsuleWindowRegistry` に自身の `AnyWindowHandle` を登録する。
- ウィンドウが閉じられると `on_window_closed` で unregister される。
- IPC ドレインループは `has_binding()` を確認し、バインディングが存在しない capsule へのディスパッチを `no_binding` エラーで拒否する。

### 4. Slug 閉集合と legacy エイリアス

canonical slug セット: `store`, `web-viewer`, `settings`, `windows`, `launch`, `identity`, `start`, `dock`, `onboarding`, `import`

旧 `ato-*` 形式は `manifest.rs` の `legacy_aliases` にのみ残し、`lookup_by_slug()` が正規化する。JS アセット（24ファイル）はすべて canonical slug へ更新済み。

### 5. Typed response / request_id

```rust
pub struct IpcRequest { pub id: Option<u64>, pub command: String, pub params: Value }
pub enum IpcResponse {
    Ok    { request_id: Option<u64>, payload: Value },
    Error { request_id: Option<u64>, code: &'static str, message: String },
}
```

JS プリロードスクリプト (`SYSTEM_IPC_INIT_SCRIPT`) が `window.__atoPendingIpc: Map<u64, resolver>` を管理し、Rust 側から `window.__atoIpcResolve(id, json)` を呼び出して Promise を解決する。

### 6. Session-scoped capsule state store

```rust
pub struct CapsuleStateStore { /* session_id → capsule_instance_key → state_key → Value */ }
```

- `capsule.state.get` / `capsule.state.set` コマンドで操作。
- session 終了時に `clear_session()` で自動破棄（O(1)）。
- 長期 persistence はスコープ外。

## Alternatives Considered

### Option A: JS 側の `capsule` フィールドを検証のみ（HMAC ベース）

- 利点: 既存コードへの変更が少ない
- 欠点: HMAC の鍵管理が複雑。WebView 内 JS が devtools から閲覧可能なため、鍵の秘密性を保証できない。Rust 側 binding による principal 解決のほうが構造的に安全。

### Option B: per-command allowlist を window ファイルに書く

- 利点: 実装が単純
- 欠点: 各ウィンドウにセキュリティポリシーが散在する。新コマンド追加のたびに複数ファイルを更新する必要があり、見落とし・policy drift が起きやすい。

### Option C: 既存 CapabilityBroker を直接拡張する

- 利点: 段階的な移行が不要
- 欠点: `CapabilityBroker` は `&mut App` に直接アクセスする実装詳細を多数持つ。抽象化せずに拡張すると、テスト可能性と将来の transport 切り替えに難が出る。

**採用**: Rust 側 binding による principal 解決（Option A/B/C を排除）+ `src/ipc/` モジュールへの分離（Option C を排除）。

## Consequences

### Good

- JS 側 `capsule` フィールドを spoof しても principal 解決に影響しない（構造的保証）。
- `IpcCommandRegistry` が単一のコマンド一覧とその visibility・capability を保持するため、権限の全量が一か所で把握できる（監査可能性）。
- Typed error により JS 側が Promise で結果を受け取れるようになった。
- `CapsuleStateStore` がセッション終了時に自動破棄されるため、メモリリークが起きない。
- ドレインループが `no_binding` を検出してエラーを返すため、閉じたウィンドウへのディスパッチが安全に拒否される。

### Bad

- `IpcBroker::dispatch` と `CapabilityBroker::dispatch` の二系統が並存している（Phase 8 時点）。`IpcBroker` はまだ stub handlers のみで本番コマンドを処理していない。実際のコマンド処理は引き続き `CapabilityBroker` が担う。
- `SystemCapsuleBinding` の `materialized_root` / `serving_root` / `version_hash` フィールドは、バンドルされた system capsule（`include_dir!` 埋め込み）には意味がない。フィールドは将来の on-disk capsule 用として定義のみ。
- 複数の launch ウィンドウが同時に存在する場合、`SystemCapsuleWindowRegistry` には最後に開かれたもののみが登録される（AtoLaunch は重複開き可能）。

## Follow-up

- [ ] `IpcBroker::dispatch` を本番ドレインループに接続し、`CapabilityBroker` を段階的に廃止する
- [ ] `IpcHandler` トレイトに `&mut App` アクセスパターンを追加する（現状 stateless）
- [ ] Guest capsule IPC (`bridge.rs`) を同じ principal / visibility モデルに統合する
- [ ] `accepted/CAPSULE_IPC_SPEC.md` を新 IPC 設計に合わせて更新する（実装検証後に別 PR）
- [ ] `AtoLaunch` の複数ウィンドウに対応した binding registry（`Vec<AnyWindowHandle>` or 上書き許容）を決定する
