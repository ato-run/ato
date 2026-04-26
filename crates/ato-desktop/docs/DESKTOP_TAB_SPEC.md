---
title: "Capsule Desktop Tab Management Architecture Specification (v1.0)"
status: accepted
date: "2026-02-07"
author: "@egamikohsuke"
ssot:
  - "apps/ato-desktop/"
related:
  - "DRAFT_LIFECYCLE.md"
  - "DESKTOP_SPEC.md"
---

# Capsule Desktop Tab Management Architecture Specification

## 1. 概要 (Overview)

### 1.1 解決する問題

ato-desktopにおけるタブ切り替え時のユーザー体験とリソース管理の最適化。

| 問題 | 現状 | 目標 |
|------|------|------|
| **状態消失** | タブ切り替えでフォーム入力・スクロール位置が失われる | JS状態を維持 |
| **メモリ管理** | 単一iframeでは重いタブが全体を圧迫 | プロセス分離で堅牢化 |
| **UXの一貫性** | 再読み込みによるストレス | ブラウザ的なタブ体験 |

### 1.2 設計方針

- **アーキテクチャ層とポリシー層の分離**
  - Architecture: Multi-Webviewによるプロセス分離
  - Governance: リソース監視ベースの動的管理
- **OSの力を借りる**: OSレベルのメモリ圧縮・スワップ機能を活用
- **ゲスト改修不要**: 既存アプリに対して透過的

---

## 2. コアアーキテクチャ

### 2.1 三層構成

```
┌─────────────────────────────────────────────────────────────────┐
│  Layer 3: UI Presentation (React)                               │
│  - タブバー、ステージ、スクリーンショット表示                      │
│  - タブ状態管理 (useOSState)                                     │
├─────────────────────────────────────────────────────────────────┤
│  Layer 2: Governance (Rust)                                     │
│  - リソース監視 (CPU/RAM)                                        │
│  - Freeze/Kill/Restore 判断ロジック                              │
│  - Screenshotキャッシュ管理                                      │
├─────────────────────────────────────────────────────────────────┤
│  Layer 1: WebView Management (Tauri v2)                         │
│  - Multi-Webview制御                                            │
│  - プロセス分離・OSメモリ管理委譲                                 │
│  - WebView <-> Rust IPC                                         │
└─────────────────────────────────────────────────────────────────┘
```

### 2.2 状態遷移モデル

```
┌──────────────┐    Switch To     ┌──────────────┐
│   Active     │◄────────────────►│  Background  │
│  (Running)   │                  │  (Freeze)    │
└──────────────┘                  └──────────────┘
       │                                  │
       │ Resource Pressure                │ Memory OK
       ▼                                  ▼
┌──────────────┐                  ┌──────────────┐
│  Suspended   │                  │   Killed     │
│(Screenshot)  │                  │(Screenshot)  │
└──────────────┘                  └──────────────┘
       │                                  │
       │ User Returns                     │ User Returns
       ▼                                  ▼
┌──────────────┐                  ┌──────────────┐
│  Restoring   │                  │  Reloading   │
│(From Cache)  │                  │(From URL)    │
└──────────────┘                  └──────────────┘
```

---

## 3. 詳細仕様

### 3.1 Layer 1: Multi-Webview Architecture

**必須条件 (Enabler)**

```rust
// src-tauri/src/tabs/webview_manager.rs
pub struct WebViewManager {
    /// Active WebView instances
    webviews: HashMap<TabId, WebViewInstance>,
    
    /// Maximum concurrent WebViews (soft limit)
    max_concurrent: usize,
    
    /// Tauri window handle
    window: Window,
}

pub struct WebViewInstance {
    pub id: TabId,
    pub webview: WebView,
    pub state: WebViewState,
    pub created_at: Instant,
    pub last_accessed: Instant,
    pub memory_usage: MemoryStats,
}

pub enum WebViewState {
    Active,      // 前面表示、完全稼働
    Frozen,      // バックグラウンド、OS圧縮対象
    Suspended,   // スクリーンショット化、WebView破棄
    Killed,      // 完全破棄、次回リロード
}
```

**Tauri v2 Multi-Webview API**

```rust
// WebView作成
let webview = window.add_child(
    WebviewBuilder::new(label, WebviewUrl::External(url))
        .auto_resize(true)
        .accept_first_mouse(true)
);

// WebView破棄（メモリ解放）
window.remove_child(&webview);
```

**実装上の注意**
- Tauri v2の`multiwebview`機能は現在unstableだが、プロセス分離のため必須
- WebViewごとに独立したメモリ空間（ChromiumのSite Isolation）
- OSは非アクティブWebViewを自動的に圧縮

### 3.2 Layer 2: Resource Governance

**リソース監視**

```rust
// src-tauri/src/tabs/governance.rs
pub struct ResourceGovernor {
    /// メモリ閾値 (MB)
    memory_threshold: usize,
    
    /// CPU使用率閾値 (%)
    cpu_threshold: f32,
    
    /// 監視間隔 (ms)
    check_interval: Duration,
}

impl ResourceGovernor {
    /// 定期的なリソースチェック
    pub async fn monitor(&self, manager: &mut WebViewManager) {
        loop {
            let stats = self.collect_system_stats();
            
            if stats.memory_used_mb > self.memory_threshold {
                // メモリ圧迫時: 最も重いタブをSuspend
                self.suspend_heaviest_tab(manager).await;
            }
            
            tokio::time::sleep(self.check_interval).await;
        }
    }
    
    /// 重いタブの特定とSuspend
    async fn suspend_heaviest_tab(&self, manager: &mut WebViewManager) {
        let heaviest = manager.webviews
            .values()
            .filter(|w| w.state == WebViewState::Frozen)
            .max_by_key(|w| w.memory_usage.total_mb);
            
        if let Some(tab) = heaviest {
            self.suspend_tab(manager, tab.id).await;
        }
    }
}
```

**状態遷移ロジック**

```rust
pub async fn on_tab_switch(
    &mut self,
    from: TabId,
    to: TabId,
) -> Result<()> {
    // 1. 現在のタブをFreeze（OSにメモリ圧縮を委譲）
    self.freeze_tab(from).await?;
    
    // 2. ターゲットタブをActivate
    if self.is_suspended(to) {
        // Suspendedの場合: スクリーンショット表示しつつ復元
        self.show_screenshot(to);
        self.restore_tab(to).await?;
    } else if self.is_killed(to) {
        // Killedの場合: 新規WebView作成
        self.create_webview(to).await?;
    } else {
        // Frozenの場合: 単にActivate
        self.activate_tab(to).await?;
    }
    
    // 3. リソースチェック
    self.enforce_limits().await?;
    
    Ok(())
}
```

### 3.3 Layer 3: Screenshot Management

**スクリーンショット取得**

```rust
// WebViewがSuspendされる直前に取得
pub async fn capture_screenshot(&self, tab_id: TabId) -> Result<Screenshot> {
    let webview = self.get_webview(tab_id)?;
    
    // TauriのScreenshot API
    let screenshot = webview.screenshot()?;
    
    // 圧縮して保存
    let compressed = compress_image(screenshot, ImageFormat::WebP, 0.8)?;
    
    let screenshot = Screenshot {
        tab_id,
        data: compressed,
        captured_at: Instant::now(),
        dimensions: (1920, 1080),
    };
    
    self.screenshot_cache.insert(tab_id, screenshot);
    Ok(())
}
```

**スクリーンショット表示**

```typescript
// Stage.tsx
interface TabStageProps {
  tab: Tab;
  state: TabState;
}

const TabStage: React.FC<TabStageProps> = ({ tab, state }) => {
  if (state === 'suspended' || state === 'killed') {
    // WebViewの代わりにスクリーンショット表示
    return (
      <div className="tab-stage">
        <Screenshot 
          src={tab.screenshotUrl} 
          isLoading={state === 'restoring'}
        />
        {state === 'restoring' && <LoadingOverlay />}
      </div>
    );
  }
  
  // Active/Frozen時は通常のWebView
  return <WebView tabId={tab.id} src={tab.url} />;
};
```

### 3.4 Frontend State Management

**タブ状態の拡張**

```typescript
// types/os.ts
interface Tab {
  id: string;
  appId: AppId;
  title: string;
  icon?: ReactNode;
  isPinned?: boolean;
  
  // 新規フィールド
  state: TabRuntimeState;
  screenshotUrl?: string;
  memoryUsage?: MemoryStats;
  lastAccessed: number;
}

type TabRuntimeState = 
  | 'active'      // 前面表示
  | 'frozen'      // バックグラウンド（WebView生存）
  | 'suspended'   // スクリーンショット化（WebView破棄、即復元可能）
  | 'killed'      // 完全破棄（次回リロード）
  | 'restoring';  // 復元中
```

**状態管理フック**

```typescript
// hooks/useTabs.ts
export const useTabs = () => {
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [activeTabId, setActiveTabId] = useState<string>();
  const { resourceStats } = useResourceMonitor();
  
  const switchTab = async (tabId: string) => {
    const currentTab = tabs.find(t => t.id === activeTabId);
    const targetTab = tabs.find(t => t.id === tabId);
    
    if (!targetTab) return;
    
    // バックエンドにタブ切り替えを通知
    await invoke('switch_tab', {
      from: currentTab?.id,
      to: targetTab.id,
    });
    
    // 楽観的UI更新
    setActiveTabId(tabId);
    updateTabState(tabId, 'active');
    if (currentTab) {
      updateTabState(currentTab.id, 'frozen');
    }
  };
  
  // リソース状態に応じた自動管理
  useEffect(() => {
    if (resourceStats.memoryUsage > 0.8) { // 80%以上
      // 最も古い非アクティブタブをSuspend
      const oldestInactive = findOldestInactiveTab(tabs);
      if (oldestInactive) {
        invoke('suspend_tab', { tabId: oldestInactive.id });
      }
    }
  }, [resourceStats]);
  
  return { tabs, activeTabId, switchTab };
};
```

---

## 4. 実装ロードマップ

### Phase 1: 基盤構築 (Week 1-2)

**Tauri Multi-Webview統合**
- [ ] `WebViewManager`の実装
- [ ] 基本的なタブ作成/破棄機能
- [ ] WebView<->Rust IPCの確立

**最小構成のタブ管理**
- [ ] Multi-Webviewでのタブ表示
- [ ] タブ切り替え（Active/Frozenのみ）
- [ ] 基本メモリ監視

### Phase 2: ガバナンス実装 (Week 3-4)

**リソース監視システム**
- [ ] `ResourceGovernor`の実装
- [ ] メモリ閾値による自動Suspend
- [ ] Screenshot取得・キャッシュ

**状態遷移の完全実装**
- [ ] Suspend/Killロジック
- [ ] Restore/Reopenフロー
- [ ] エラーハンドリング

### Phase 3: UI/UX強化 (Week 5-6)

**フロントエンド統合**
- [ ] Screenshot表示コンポーネント
- [ ] ローディング状態のUX
- [ ] メモリ使用状況の可視化

**最適化**
- [ ] Screenshot圧縮最適化
- [ ] キャッシュLRU管理
- [ ] パフォーマンスチューニング

### Phase 4: 安定化 (Week 7-8)

**テスト・検証**
- [ ] メモリリーク検証
- [ ] 長時間運転テスト
- [ ] 異常系テスト

**Tauri v2対応**
- [ ] multiwebviewの安定化対応
- [ ] プラットフォーム別調整

---

## 5. 制約と前提条件

### 5.1 Tauri v2の制約

| 項目 | 現状 | 対応 |
|------|------|------|
| multiwebview | unstable | 早期導入、issue追跡 |
| Windows | 完全対応 | 本番利用可能 |
| macOS | 完全対応 | 本番利用可能 |
| Linux | 制限あり | WebKitGTK依存 |

### 5.2 メモリ前提

**想定シナリオ**
- 平均的なWebアプリ: 50-100MB/WebView
- 重いアプリ（地図等）: 200-300MB/WebView
- 同時Active: 1タブ
- 同時Frozen: 2-5タブ（OS圧縮で1/3程度）
- Suspend: 無制限（スクリーンショットのみ: ~100KB/タブ）

**推奨最小スペック**
- RAM: 8GB
- ストレージ: Screenshotキャッシュ用に1GB

---

## 6. 代替案・将来拡張

### 6.1 DOM Snapshot（将来検討）

**条件**
- CSPの緩いアプリのみ対象
- rrweb-snapshot等のライブラリ活用
- Canvas/WebGLを使わないアプリ限定

**実装タイミング**
- Phase 3以降のオプション機能
- デフォルトはScreenshot方式

### 6.2 State Sync（将来検討）

**条件**
- Capsule SDK対応アプリのみ
- 明示的な状態保存API

**実装タイミング**
- SDK普及後

---

## 7. 参考実装・ベストプラクティス

### 7.1 Chromeのタブ管理

Chromeは以下の戦略を採用:
1. **Discard**: メモリ不足時にタブを破棄（スクリーンショット保持）
2. **Freeze**: バックグラウンドタブのJavaScript実行を抑制
3. **Compression**: OSのメモリ圧縮機能活用

本仕様は同様のアプローチをRust/Tauriで再現。

### 7.2 メモリ監視パターン

```rust
// Chromeのdiscardingアルゴリズム（参考）
fn should_discard_tab(tab: &Tab, system: &SystemStats) -> bool {
    if system.available_memory_mb < 100 {
        return true; // 緊急時
    }
    
    if tab.is_pinned || tab.is_playing_audio {
        return false; // 保護対象
    }
    
    // 最も古く、最も重いタブを優先
    let score = tab.last_accessed.elapsed().as_secs() as f32 
                * tab.memory_usage_mb as f32;
    
    score > THRESHOLD
}
```

---

## 8. 関連ドキュメント

- [DESKTOP_SPEC.md](DESKTOP_SPEC.md) - Desktop全体仕様
- [DRAFT_LIFECYCLE.md](DRAFT_LIFECYCLE.md) - Lifecycle管理
- [APP_IN_APP.md](APP_IN_APP.md) - Guest Mode仕様
- [CAPSULE_IPC_SPEC](DRAFT_CAPSULE_IPC.md) - IPC仕様

---

## 9. 変更履歴

| 日付 | バージョン | 変更内容 | 担当 |
|------|-----------|----------|------|
| 2026-02-07 | v1.0 | 初版作成 | Architecture Team |

---

## 10. 承認

- [ ] 技術責任者承認
- [ ] セキュリティレビュー
- [ ] 実装チーム合意
