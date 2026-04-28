---
title: "Host Panel Routing"
status: draft
date: 2026-04-29
author: "@koh0920"
ssot:
  - "crates/ato-desktop/src/state/mod.rs"
  - "crates/ato-desktop/src/state/persistence.rs"
  - "crates/ato-desktop/src/webview.rs"
  - "crates/ato-desktop/src/ui/mod.rs"
  - "crates/ato-desktop/src/ui/panels/mod.rs"
related:
  - "accepted/DESKTOP_TAB_SPEC.md"
  - "accepted/MULTI_WEBVIEW_SPEC.md"
  - "draft/PURE_TRANSFORMS_AND_LOCK_LAYERS.md"
---

# Host Panel Routing

## 1. 概要

GPUI host shell を維持したまま、launcher / settings / capsule detail を Wry + React の
host-owned panel として段階移行するための routing / surface / persistence 契約を定義する。

本 RFC は Phase 1 実装着手前の design gate であり、次の 3 点を先に固定する。

- host panel を capsule guest とは別の surface / route / bridge として扱う
- overlay ベースの panel を pane / task ベースの model に寄せる
- persistence と asset build の境界を先に決め、PR ごとの責務を明確にする

## 2. スコープ

### スコープ内

- `HostPanelRoute` と `PaneSurface::HostPanel(...)` の導入方針
- host panel 専用 WebView builder の分離
- settings / launcher / capsule detail の pane semantics
- host panel の persistence 契約
- `capsule-host://` asset serving と `frontend/dist` 依存の有効化タイミング
- host bridge の信頼境界と security gate

### スコープ外

- React 側の UI 実装詳細
- component library 選定
- dynamic theme customization
- mobile / Swift UI 統合
- capsule detail route key の `pane_id` 以外への移行

## 3. 設計

### 3.1 Surface Model

host panel は guest webview と同じ `PaneSurface::Web` に混ぜず、独立 variant として表現する。

```rust
pub enum HostPanelRoute {
    Launcher,
    Settings { section: Option<SettingsSection> },
    CapsuleDetail { pane_id: PaneId, tab: DetailTab },
}

pub enum PaneSurface {
    Web(WebPane),
    HostPanel(HostPanelRoute),
    // ...existing variants
}
```

原則:

- `PaneSurface::Web` は untrusted な capsule guest 専用
- `PaneSurface::HostPanel` は Desktop 自身が所有する trusted panel 専用
- Rust 側の `HostPanelRoute` が canonical state であり、`capsule-host://...` URL は
  その projection として扱う

型名 `SettingsSection` / `DetailTab` は route-level の概念名である。初期実装では既存の
`SettingsTab` / `CapsuleDetailTab` を type alias または変換 layer で流用してよい。

### 3.2 WebView Builder Separation

guest と host panel は WebView builder を完全分離する。

```rust
fn build_guest_webview(...) -> Result<ManagedWebView>
fn build_host_panel_webview(...) -> Result<ManagedWebView>
```

分離する理由:

- preload namespace を分けるため
- custom protocol / IPC handler / navigation policy を分けるため
- guest 側へ `__atoHost__` が leak しないことを構造上保証するため

共通化してよいのは bounds 計算、user agent suffix、shared helper などの機械的部分のみで、
次は共通化しない。

- protocol registration
- preload injection
- IPC request decoding
- navigation handler
- page-load ready policy

### 3.3 Pane / Task Semantics

overlay から pane へ寄せる際の user-facing semantics は先に固定する。

#### Settings

- singleton task とする
- sidebar button と omnibar suggestion は同じ task に focus する
- route は `HostPanelRoute::Settings { section }`
- 復元対象に含める

#### Launcher

- `HostPanelRoute::Launcher` を使うが、transient 扱いとする
- 新規タブの初期 surface として開く
- 復元対象には含めない

#### Capsule Detail

- `HostPanelRoute::CapsuleDetail { pane_id, tab }` を使う
- pane ごとに独立 task とする
- close しても元 pane は残る
- 復元対象には含めない

#### Overlay Policy

host-owned persistent panel は pane 化し、GPUI overlay は transient UI のみに限定する。

- toast
- context menu
- confirm dialog
- ephemeral modal

### 3.4 Persistence Contract

`state/persistence.rs` は host panel route を明示的に知る必要がある。v0 の persistence 契約は次の通り。

```rust
enum PersistedRoute {
    ExternalUrl { url: String },
    CapsuleHandle { handle: String, label: String },
    CapsuleUrl { handle: String, label: String, url: String },
    HostPanel(PersistedHostPanelRoute),
}

enum PersistedHostPanelRoute {
    Settings { section: Option<String> },
}
```

ルール:

- `Settings` は serialize / deserialize の対象
- `Launcher` は serialize しない
- `CapsuleDetail` は serialize しない
- deserialize 時に未知の host panel variant は drop する
- `CapsuleDetail` は capsule lifecycle に従属するため、孤児化回避を優先して非復元で固定する

この契約により、settings だけが stable route として restart 後に戻り、launcher と detail は
transient panel として扱われる。

### 3.5 Asset Build / Dist Contract

frontend bundle と Rust build を結ぶタイミングは PR 単位で段階化する。

#### PR1

- `crates/ato-desktop/frontend/` を新設する
- `xtask frontend build` / `xtask frontend dev` を導入する
- Rust build はまだ frontend に依存しない

#### PR2

- `capsule-host://` asset serving を導入する
- この時点で初めて `frontend/dist` を Rust 側の入力として扱う
- `cargo build` 時に `frontend/dist` が無ければ明示 error を返す
- `ATO_DESKTOP_SKIP_FRONTEND_BUILD=1` は `dist` が既に存在する場合にだけ有効

明示 error の目的は「暗黙 build を避ける」ことであり、`build.rs` から Node build を自動起動することではない。

### 3.6 Dev URL Contract

開発時は `ATO_DESKTOP_FRONTEND_DEV_URL` を使って dev server を指せるようにする。

ただし dev mode でも信頼境界は変えない。

- host bridge injection は有効
- origin 制限は有効
- host navigation policy は有効
- `http(s)` への自由遷移は許可しない

## 4. インターフェース

### 4.1 `capsule-host://`

`capsule-host://` は host panel 静的 assets 専用の custom protocol とする。

用途:

- bundled `frontend/dist` の配信
- host panel の entrypoint 配信
- dev server proxy の entrypoint

禁止事項:

- capsule guest asset の配信
- external `http(s)` asset の透過配信
- guest bridge の多重流用

### 4.2 Host Bridge

host bridge は guest bridge から独立した typed IPC とする。

初期コマンド:

- `host.ping`
- `host.getRoute`
- `host.navigate`
- `host.close`
- `host.subscribe`
- `host.unsubscribe`

push 経路:

- host -> webview は `evaluate_script` による `window.__atoHost__.__push(channel, payload)`

`host.subscribe` / `host.unsubscribe` は Phase 3 で detail logs を流すための先行基盤として、
Phase 1 から存在してよい。

## 5. セキュリティ

host panel は trusted surface なので、guest より強い保護を先に固定する。

### 5.1 Injection Boundary

- `window.__atoHost__` は `build_host_panel_webview()` でのみ注入する
- guest webview には絶対に注入しない
- test で guest webview 上の `window.__atoHost__ === undefined` を確認する

### 5.2 IPC Origin Gate

- host bridge IPC は `capsule-host://` origin 以外から受け付けない
- origin 判定できない request は reject する
- typed command decoder は host bridge 専用 enum を使う

### 5.3 Navigation Policy

- host panel webview は `capsule-host://` のみ許可する
- `http://` / `https://` への遷移は reject する
- dev URL 使用時も、許可対象は構成された frontend dev server origin に限定する

### 5.4 CSP

host panel 向け CSP は guest とは独立して定義する。

最低要件:

- external fetch 禁止
- inline script 禁止
- `script-src 'self'`
- dev server mode 時のみ必要最小限の例外を追加

## 6. 既知の制限

- `CapsuleDetail` の route key は Phase 1 では `pane_id` を使う
- `session_id` / `canonical_handle` への移行は future work とし、現段階の blocker にはしない
- settings 以外の host panel persistence は扱わない
- launcher は pane 化されても transient 扱いを維持する

## 7. 実装順序

1. doc PR: 本 RFC を追加し、型 / builder / persistence / dist timing を固定する
2. PR1: frontend skeleton + xtask
3. PR2: `capsule-host://` asset serving + dist error 有効化 + dev URL
4. PR3: host bridge + security gate + subscribe / unsubscribe
5. PR4: design token SSOT + build-time codegen

## 参照

- `crates/ato-desktop/src/state/mod.rs` — pane / task / route state の現行 SSOT
- `crates/ato-desktop/src/state/persistence.rs` — tab restore の現行境界
- `crates/ato-desktop/src/webview.rs` — guest webview builder と custom protocol の現行実装
- `crates/ato-desktop/src/ui/mod.rs` — settings / detail overlay の現行差し込み位置
- `crates/ato-desktop/src/ui/panels/mod.rs` — launcher stage pane の現行実装