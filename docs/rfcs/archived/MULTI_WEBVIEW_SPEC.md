---
title: "Multi-Webview Architecture Specification (v1.0)"
status: accepted
date: "2026-02-07"
author: "@egamikohsuke"
ssot:
  - "apps/ato-desktop/"
related:
  - "DESKTOP_TAB_SPEC.md"
---

# Multi-Webview Architecture Specification

## 1. 概要 (Overview)

### 1.1 背景と目的

ato-desktop の iframe から webview への移行において、以下の問題が発生していた：

- **HiDPI/Retina スケーリング問題**: Webview の拡大・位置ズレ
- **OSレベルでのプロセス分離の欠如**: サイドバーとメインコンテンツが同じプロセス
- **状態管理の分散**: React 側と Rust 側で二重管理

本仕様は、Tauri v2 の Multi-Webview 機能を活用し、**Rust-driven アーキテクチャ** でこれらを解決する。

### 1.2 設計方針

- **Single Source of Truth**: すべての状態を Rust の AppState で管理
- **完全なプロセス分離**: サイドバーとメインを別 Webview（別プロセス）で配置
- **URLベースの分離**: react-router-dom を使用し、1つの React アプリで複数ルートを提供
- **将来の拡張性**: Dock 化などの UI 変更に強い設計

---

## 2. コアアーキテクチャ

### 2.1 システム構成図

```
┌─────────────────────────────────────────────────────────────────┐
│  Rust Main Process                                               │
│  ┌───────────────┐  ┌───────────────┐  ┌──────────────────┐    │
│  │   AppState    │  │ LayoutManager │  │   Event Bus      │    │
│  │   (Mutex)     │  │               │  │                  │    │
│  │ - tabs        │  │ - resize      │  │ - tab-changed    │    │
│  │ - active_tab  │  │ - layout      │  │ - state-updated  │    │
│  └───────┬───────┘  └───────┬───────┘  └────────┬─────────┘    │
│          │                  │                   │              │
│          └──────────────────┴───────────────────┘              │
│                              │                                  │
│                    ┌─────────┴─────────┐                       │
│                    │   Tauri Commands  │                       │
│                    │   - get_app_state │                       │
│                    │   - create_tab    │                       │
│                    │   - switch_tab    │                       │
│                    │   - close_tab     │                       │
│                    └───────────────────┘                       │
└─────────────────────────────────────────────────────────────────┘
                                    │
                    ┌───────────────┼───────────────┐
                    │               │               │
                    ▼               │               ▼
┌───────────────────────┐          │   ┌───────────────────────┐
│  WebView A: Sidebar   │          │   │  WebView B: Main      │
│  ┌─────────────────┐  │          │   │  ┌─────────────────┐  │
│  │  React Router   │  │          │   │  │  React Router   │  │
│  │  /sidebar route │  │          │   │  │  / (main) route │  │
│  │                 │  │          │   │  │                 │  │
│  │  ArcSidebar     │  │          │   │  │  Stage          │  │
│  │  Component      │  │          │   │  │  Component      │  │
│  │                 │  │          │   │  │                 │  │
│  │  - Tab list     │  │          │   │  │  - Dashboard    │  │
│  │  - Click ->     │  │          │   │  │  - Settings     │  │
│  │    IPC Command  │  │          │   │  │  - Capsule      │  │
│  └─────────────────┘  │          │   │  └─────────────────┘  │
└───────────────────────┘          │   └───────────────────────┘
                    │               │               │
                    └───────────────┴───────────────┘
                                    │
                                    ▼
                          ┌─────────────────┐
                          │  Vite DevServer │
                          │  (Development)  │
                          │  or             │
                          │  Static Files   │
                          │  (Production)   │
                          └─────────────────┘
```

### 2.2 レイアウト仕様

| WebView | 位置 | サイズ | URL |
|---------|------|--------|-----|
| Sidebar | x: 0, y: 0 | width: 280px (fixed), height: 100% | `/sidebar` |
| Main | x: 280px, y: 0 | width: remaining, height: 100% | `/` |

**計算ロジック:**
```rust
let scale_factor = window.scale_factor()?;
let window_size = window.inner_size()?; // Physical pixels

let sidebar_width = (280.0 * scale_factor).round() as u32;

// Sidebar
sidebar.set_bounds(Rect {
    position: Position::Physical(PhysicalPosition { x: 0, y: 0 }),
    size: Size::Physical(PhysicalSize { 
        width: sidebar_width, 
        height: window_size.height 
    }),
})?;

// Main
main.set_bounds(Rect {
    position: Position::Physical(PhysicalPosition { 
        x: sidebar_width as i32, 
        y: 0 
    }),
    size: Size::Physical(PhysicalSize { 
        width: window_size.width - sidebar_width, 
        height: window_size.height 
    }),
})?;
```

---

## 3. 状態管理仕様

### 3.1 AppState (Rust)

```rust
use std::sync::Mutex;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    pub id: String,
    pub title: String,
    pub url: Option<String>,
    pub favicon: Option<String>,
    pub is_pinned: bool,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppState {
    pub tabs: Vec<Tab>,
    pub active_tab_id: Option<String>,
    pub sidebar_width: u32, // pixels
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            tabs: vec![],
            active_tab_id: None,
            sidebar_width: 280,
        }
    }
}
```

### 3.2 State Management Pattern

**React → Rust (Commands):**
- `get_app_state() -> AppState` - 初期状態取得
- `create_tab(url: String, title: String) -> Tab` - 新規タブ作成
- `switch_tab(tab_id: String) -> ()` - タブ切り替え
- `close_tab(tab_id: String) -> ()` - タブクローズ
- `update_tab(tab_id: String, updates: TabUpdate) -> Tab` - タブ更新

**Rust → React (Events):**
- `tab-changed { tab_id: String }` - アクティブタブ変更
- `state-updated { state: AppState }` - 状態全体の更新
- `tab-created { tab: Tab }` - 新規タブ作成通知
- `tab-closed { tab_id: String }` - タブクローズ通知

---

## 4. フロントエンド仕様

### 4.1 ルーティング構成

```typescript
// App.tsx
import { BrowserRouter, Routes, Route } from 'react-router-dom';

function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route path="/sidebar" element={<SidebarRoute />} />
        <Route path="/*" element={<MainRoute />} />
      </Routes>
    </BrowserRouter>
  );
}

function SidebarRoute() {
  // サイドバーのみ表示
  return <ArcSidebar />;
}

function MainRoute() {
  // メインコンテンツ（ダッシュボード、設定、カプセル等）
  return <Stage />;
}
```

### 4.2 React Hooks

```typescript
// hooks/useTauriState.ts
export function useTauriState() {
  const [state, setState] = useState<AppState | null>(null);
  
  useEffect(() => {
    // 初期状態取得
    commands.getAppState().then(setState);
    
    // イベントリスナー
    const unlisten = listen('state-updated', (event) => {
      setState(event.payload);
    });
    
    return () => { unlisten.then(f => f()); };
  }, []);
  
  const switchTab = useCallback((tabId: string) => {
    commands.switchTab(tabId);
  }, []);
  
  const createTab = useCallback((url: string, title: string) => {
    commands.createTab(url, title);
  }, []);
  
  return { state, switchTab, createTab };
}
```

### 4.3 既存コンポーネントの修正点

**ArcSidebar.tsx:**
- 状態管理: `useOSState` → `useTauriState`
- タブクリック: 直接 state 更新 → `switchTab(tabId)` command 呼び出し
- 削除: Stage への参照、直接の状態変更

**Stage.tsx:**
- 状態管理: `useOSState` → `useTauriState`
- カプセル表示: iframe → native webview（別管理）
- タブ切り替え: イベントリスナーで検知

---

## 5. Rust 実装仕様

### 5.1 セットアップ時の Webview 作成

```rust
// src-tauri/src/lib.rs

#[tauri::command]
async fn setup_multi_webview(app: &tauri::App) -> Result<(), String> {
    let window = app.get_webview_window("main")
        .ok_or("Main window not found")?;
    
    // Sidebar WebView
    let sidebar = WebviewBuilder::new("sidebar", WebviewUrl::App("/sidebar".into()))
        .transparent(false)
        .build(&window)?;
    
    // Main WebView
    let main_view = WebviewBuilder::new("main_view", WebviewUrl::App("/".into()))
        .transparent(false)
        .build(&window)?;
    
    // 初期レイアウト
    apply_layout(&window, &sidebar, &main_view)?;
    
    // リサイズイベント
    let w = window.clone();
    let s = sidebar.clone();
    let m = main_view.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::Resized(_) = event {
            let _ = apply_layout(&w, &s, &m);
        }
    });
    
    Ok(())
}

fn apply_layout(
    window: &tauri::Window,
    sidebar: &tauri::Webview,
    main: &tauri::Webview,
) -> Result<(), tauri::Error> {
    let scale_factor = window.scale_factor()?;
    let size = window.inner_size()?;
    
    let sidebar_width = (280.0 * scale_factor).round() as u32;
    
    sidebar.set_bounds(tauri::Rect {
        position: tauri::Position::Physical(tauri::PhysicalPosition { x: 0, y: 0 }),
        size: tauri::Size::Physical(tauri::PhysicalSize { 
            width: sidebar_width, 
            height: size.height 
        }),
    })?;
    
    main.set_bounds(tauri::Rect {
        position: tauri::Position::Physical(tauri::PhysicalPosition { 
            x: sidebar_width as i32, 
            y: 0 
        }),
        size: tauri::Size::Physical(tauri::PhysicalSize { 
            width: size.width.saturating_sub(sidebar_width), 
            height: size.height 
        }),
    })?;
    
    Ok(())
}
```

### 5.2 コマンド実装

```rust
// src-tauri/src/commands/app_state.rs

use std::sync::Mutex;
use tauri::State;

pub struct AppStateStore(Mutex<AppState>);

#[tauri::command]
#[specta::specta]
pub fn get_app_state(state: State<AppStateStore>) -> AppState {
    state.0.lock().unwrap().clone()
}

#[tauri::command]
#[specta::specta]
pub fn create_tab(
    url: String,
    title: String,
    state: State<AppStateStore>,
    app: tauri::AppHandle,
) -> Result<Tab, String> {
    let tab = Tab {
        id: format!("tab-{}", uuid::Uuid::new_v4()),
        title,
        url: Some(url),
        favicon: None,
        is_pinned: false,
        created_at: chrono::Utc::now().timestamp_millis(),
    };
    
    {
        let mut s = state.0.lock().unwrap();
        s.tabs.push(tab.clone());
        s.active_tab_id = Some(tab.id.clone());
    }
    
    // イベント発行
    app.emit("tab-created", &tab)?;
    app.emit("tab-changed", &tab.id)?;
    
    Ok(tab)
}

#[tauri::command]
#[specta::specta]
pub fn switch_tab(
    tab_id: String,
    state: State<AppStateStore>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut s = state.0.lock().unwrap();
        if !s.tabs.iter().any(|t| t.id == tab_id) {
            return Err("Tab not found".to_string());
        }
        s.active_tab_id = Some(tab_id.clone());
    }
    
    app.emit("tab-changed", &tab_id)?;
    Ok(())
}
```

---

## 6. 実装フェーズ

### Phase 1: Rust State 基盤 (1-2日)

1. **AppState 構造体の定義**
   - File: `src-tauri/src/state/mod.rs`
   - タブ構造体、状態構造体

2. **Tauri Commands 実装**
   - File: `src-tauri/src/commands/app_state.rs`
   - get_app_state, create_tab, switch_tab, close_tab

3. **イベント発行実装**
   - tab-changed, state-updated, tab-created, tab-closed

4. **IPC テスト**
   - フロントエンドからコマンド呼び出しテスト

### Phase 2: フロントエンド分離 (1-2日)

1. **react-router-dom 導入**
   - File: `src/App.tsx`
   - ルーティング設定（/sidebar, /）

2. **useTauriState フック作成**
   - File: `src/hooks/useTauriState.ts`
   - Rust との双方向通信

3. **ArcSidebar 修正**
   - File: `src/components/layout/ArcSidebar.tsx`
   - useOSState → useTauriState
   - IPC command 呼び出しに変更

4. **Stage 修正**
   - File: `src/components/layout/Stage.tsx`
   - useOSState → useTauriState
   - イベントリスナー実装

### Phase 3: Multi-Webview 統合 (2-3日)

1. **セットアップ時 Webview 作成**
   - File: `src-tauri/src/lib.rs`
   - Sidebar と Main の Webview 生成

2. **レイアウトロジック実装**
   - File: `src-tauri/src/layout/manager.rs`
   - apply_layout 関数
   - スケールファクター対応

3. **リサイズイベント処理**
   - WindowEvent::Resized ハンドラ
   - 自動再レイアウト

4. **Webview 間通信**
   - Sidebar → Main へのイベント伝達

### Phase 4: カプセル統合 (1-2日)

1. **カプセル Webview 管理**
   - 既存の `tabs/webview_manager.rs` を統合
   - カプセル用 Webview を Main エリアに配置

2. **状態同期**
   - カプセル起動時のタブ作成
   - カプセル URL の Main Webview への反映

3. **動作確認**
   - サイドバーからカプセル起動
   - タブ切り替え
   - ウィンドウリサイズ

---

## 7. 技術制約と対応

### 7.1 Tauri v2 Multiwebview の制約

| 制約 | 対応 |
|------|------|
| macOS で physical position が不正確 | LogicalPosition は使わず、set_bounds で PhysicalPosition を使用 |
| auto_resize と位置固定の競合 | auto_resize(false) にし、手動でレイアウト制御 |
| リサイズ時の座標計算 | scale_factor を考慮した計算を毎回実行 |

### 7.2 既存コードとの互換性

- **useOSState**: 段階的に移行、最初は useTauriState のラッパーとしても可
- **カプセル管理**: 既存の `tabs` モジュールを統合、競合しないよう設計
- **イベント**: 既存の `capsule-session-update` なども維持

---

## 8. テスト計画

### 8.1 機能テスト

- [ ] Sidebar Webview が正しい位置・サイズで表示される
- [ ] Main Webview が正しい位置・サイズで表示される
- [ ] ウィンドウリサイズ時に両方の Webview が追従する
- [ ] タブ作成時に Sidebar と Main が同期する
- [ ] タブ切り替え時に Main の表示が変わる
- [ ] カプセル起動時に正しい URL が表示される

### 8.2 HiDPI テスト

- [ ] Retina ディスプレイ (scale_factor=2.0) で正しく表示
- [ ] 外部モニター (scale_factor=1.0) で正しく表示
- [ ] 異なるスケールのモニター間でウィンドウ移動しても正しく表示

### 8.3 パフォーマンステスト

- [ ] 10タブ以上開いてもスムーズに動作
- [ ] 連続リサイズでも問題なく動作
- [ ] メモリ使用量が適切

---

## 9. 将来の拡張

### 9.1 Dock 化

将来的にサイドバーを別ウィンドウとして切り離せるようにする場合：

```rust
// 将来的な実装例
fn undock_sidebar(app: &AppHandle) -> Result<(), Error> {
    let sidebar_window = WebviewWindowBuilder::new(
        app,
        "sidebar-floating",
        WebviewUrl::App("/sidebar".into())
    )
    .position(x, y)
    .size(280, 600)
    .build()?;
    
    // 元のウィンドウから Sidebar Webview を削除
    // Main Webview をフルスクリーン化
}
```

### 9.2 ドラッグ & ドロップ

Rust 側でファイルドロップを処理し、適切な Webview に転送：

```rust
.on_window_event(|window, event| {
    if let WindowEvent::DragDrop(drop_event) = event {
        // ドロップ位置に基づいて対象の Webview を決定
        // Sidebar 領域なら Sidebar へ、Main 領域なら Main へ
    }
})
```

---

## 10. 関連ドキュメント

- [DESKTOP_TAB_SPEC.md](DESKTOP_TAB_SPEC.md) - タブ管理仕様
- [Tauri v2 Webview API](https://docs.rs/tauri/latest/tauri/webview/)
- [Tauri v2 Window Events](https://docs.rs/tauri/latest/tauri/enum.WindowEvent.html)

---

## 11. 変更履歴

| 日付 | バージョン | 変更内容 | 担当 |
|------|-----------|----------|------|
| 2026-02-07 | v1.0 | 初版作成 | Architecture Team |

---

## 12. 承認

- [ ] 技術責任者承認
- [ ] 実装チーム合意
