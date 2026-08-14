# DRAFT: Session-Owned Capsule Lifecycle

> Desktop の capsule lifecycle を、window/pane 所有から session 所有に移行する。
> この RFC は実装計画としても機能する。

## 問題設定

現在の `ato-desktop` には以下の 4 つの問題がある:

1. **セッションが一元管理されていない**
   - 実セッション (`GuestLaunchSession`) は `WebViewManager`（レガシー単一ウィンドウ）または
     `AppCapsuleShell` エンティティ（マルチウィンドウ）に分散して存在し、中央管理されていない。

2. **close 時の挙動がモード依存**
   - Focus View モード: `AppCapsuleShell::Drop` が常に `stop_guest_session` を呼ぶ
   - レガシーモード: ペインを閉じるとリテンションテーブルに降格される（5 分 TTL）
   - ユーザーが close = stop と認識していない場合、意図しないプロセス停止が起きる

3. **os-browser / headless が Open Windows で管理不能**
   - `OpenContentWindows` は GPUI ウィンドウ ID をキーにしており、
     OS ブラウザ表示セッションやヘッドレスセッションを追跡できない

4. **起動前モーダルがウィンドウ起動パスに結合している**
   - `start_boot_launch` は常に `open_ready_capsule_window`（AppWindow の作成）で終了し、
     OS ブラウザやヘッドレスにルーティングするパスがない
   - E103/E302 モーダルが `os-browser` 起動時には表示されない

## 設計原則

```
実行ライフサイクル: CapsuleSession が所有  → process_state が正
表示ライフサイクル: SessionClient が参照    → AtoWindow / OsBrowser / Headless が client として attach
```

- **window close は session stop ではない**
  - Close window = display detach、Stop session = process cleanup
- **Session と Client を分離する** — 同一 session に複数 client が attach 可能
- **Drop は停止経路にしない** — 明示的な close event / stop action で制御する
- **PendingLaunch は単一 slot にしない** — launch_id Map で複数同時起動に耐える

## コア型定義

### CapsuleSession（process lifecycle の正）

```rust
pub struct CapsuleSession {
    pub session_id: String,
    pub handle: String,
    pub canonical_handle: Option<String>,
    pub title: String,
    pub process_state: SessionProcessState,
    pub local_url: Option<String>,
    pub healthcheck_url: Option<String>,
    pub launch_context: CapsuleLaunchContext,
    pub launch_via: LaunchVia,
    pub created_at: SystemTime,
    pub last_seen_at: SystemTime,
}

pub enum SessionProcessState {
    Starting,
    Ready,
    Stopping,
    Stopped,
    FailedToStop { error: String },
}

pub enum LaunchVia {
    Cli,
    Desktop,
}
```

### SessionClient（表示の正、複数可）

```rust
pub struct SessionClient {
    pub client_id: SessionClientId,
    pub session_id: String,
    pub client_kind: SessionClientKind,
    pub window_id: Option<u64>,
    pub pane_id: Option<PaneId>,
    pub state: SessionClientState,
    pub attached_at: SystemTime,
    pub last_seen_at: SystemTime,
}

pub enum SessionClientKind {
    AtoWindow,
    WebViewPane,
    OsBrowser,
    Headless,
}

pub enum SessionClientState {
    Attached,
    Detached,
    External,
    Closing,
}
```

### CapsuleLaunchContext（Restart 用の全情報）

```rust
pub struct CapsuleLaunchContext {
    pub handle_or_url: String,
    pub target: Option<String>,
    pub requested_client: SessionClientKind,
    pub source: CapsuleOpenSource,
}

pub enum CapsuleOpenSource {
    NavigateToUrl,
    Dock,
    StartPage,
    CardSwitcher,
    Automation,
}
```

### SessionViewEntry（Open Windows 表示用、1 session = 1 row）

```rust
pub struct SessionViewEntry {
    pub session_id: String,
    pub title: String,
    pub handle: String,
    pub presentation_state: PresentationState,
    pub attached_clients: Vec<ClientSummary>,
    pub primary_window_id: Option<u64>,
    pub local_url: Option<String>,
}

pub struct ClientSummary {
    pub client_kind: SessionClientKind,
    pub state: SessionClientState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentationState {
    Failed,
    Stopped,
    Visible,
    External,
    Detached,
    Headless,
}

// PartialOrd に頼らず、明示関数で優先度を決定する
impl PresentationState {
    fn priority(self) -> u8 {
        match self {
            Self::Failed => 0,
            Self::Stopped => 1,
            Self::Visible => 2,
            Self::External => 3,
            Self::Detached => 4,
            Self::Headless => 5,
        }
    }
}
```

### SessionRegistry（`AppState` 上の単一 source of truth）

Session lifecycle の owner として、`AppState` に直接持たせる。
GPUI global は `AppState` へのアクセス経路として使う場合のみ追加する。
二重 source of truth は避ける。

```rust
// AppState に追加:
pub struct AppState {
    // ... existing fields ...
    pub sessions: SessionRegistry,
    pub pending_launches: PendingLaunches,
}

pub struct SessionRegistry {
    sessions: HashMap<String, CapsuleSession>,
    clients: HashMap<SessionClientId, SessionClient>,
    window_to_clients: HashMap<u64, Vec<SessionClientId>>,
    next_client_id: SessionClientId,
}

impl SessionRegistry {
    // 基本操作
    pub fn register_session(&mut self, session: CapsuleSession);
    pub fn attach_client(&mut self, client: SessionClient);
    pub fn remove_session(&mut self, session_id: &str);

    // Client 操作
    pub fn update_client_state(&mut self, client_id: SessionClientId, state: SessionClientState);
    pub fn detach_client(&mut self, client_id: SessionClientId);
    pub fn remove_client(&mut self, client_id: SessionClientId);

    // クエリ
    pub fn get_session(&self, session_id: &str) -> Option<&CapsuleSession>;
    pub fn clients_for_session(&self, session_id: &str) -> Vec<&SessionClient>;
    pub fn clients_by_window_id(&self, window_id: u64) -> Vec<SessionClientId>;
    pub fn session_id_for_client(&self, client_id: SessionClientId) -> Option<&str>;

    // 停止操作（二重停止防止 + UI thread 完了通知）
    pub fn stop_session_once(
        &mut self,
        session_id: &str,
        on_complete: impl FnOnce(StopResult) + Send + 'static,
    );

    // Open Windows 表示用（1 session = 1 row）
    pub fn view_entries(&self) -> Vec<SessionViewEntry>;
}
```

### PendingLaunches（複数同時起動に耐える）

```rust
pub struct PendingLaunches {
    pub launches: HashMap<LaunchRequestId, (CapsuleLaunchRequest, PendingLaunchState)>,
}

pub struct CapsuleLaunchRequest {
    pub launch_id: LaunchRequestId,
    pub handle_or_url: String,
    pub target: Option<String>,
    pub requested_client: SessionClientKind,
    pub source: CapsuleOpenSource,
    pub origin_window_id: Option<u64>,
    pub created_at: SystemTime,
}

pub enum PendingLaunchState {
    AwaitingApproval,
    ApprovedStarting,
    BlockedAgain,  // prepare_launch_session が E103/E302 で再ブロック
}
```

## PR 分割

### PR-D1: `windowCloseBehavior` config のみ

**目的**: 設定の保存・読み込み経路を追加する。動作変更なし。

#### 追加する型

```rust
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum WindowCloseBehavior {
    #[default]
    KeepSessionRunning,
    StopSession,
}
```

保存値: `keep-session-running` / `stop-session`

#### 変更ファイル

| # | ファイル | 変更内容 |
|---|------|--------|
| 1 | `config.rs` | `WindowCloseBehavior` enum 追加、`DesktopSettings.window_close_behavior` 追加、Default 更新 |
| 2 | `settings.rs` | snapshot エントリ `"windowCloseBehavior"`、patch ハンドラ、parser `parse_window_close_behavior` 追加 |
| 3 | `config.rs` tests | default、roundtrip、missing field のテスト |
| 4 | `settings.rs` tests | snapshot、patch、NEXT_LAUNCH_KEYS 非含有のテスト |

### PR-D2a: SessionRegistry スケルトン

**目的**: `CapsuleSession` + `SessionClient` + `SessionRegistry` の型と基本メソッドを定義。既存動作は一切変更しない。

#### 追加ファイル

| # | ファイル | 内容 |
|---|------|------|
| 1 | 新規 `state/session.rs` | 全型定義 + `SessionRegistry` impl + GPUI global 登録 |

#### Unit tests

```
- register_session inserts and returns by session_id
- attach_client returns different client_id for same session
- same session can have AtoWindow + OsBrowser clients simultaneously
- detach_client changes state to Detached, keeps session alive
- remove_session removes all associated clients
- clients_by_window_id returns all clients for same window
- session_id_for_client returns correct session_id
- view_entries deduplicates sessions (1 row even with 2 clients)
- view_entries: Visible wins over External when both clients exist
- view_entries: Failed wins over all other states
- process_state transitions: Starting → Ready → Stopping → Stopped
- mru_entries combines sessions and clients sorted by last_seen_at
```

### PR-D2b: 可視ウィンドウ統合

**目的**: `CapsuleSession` と `SessionClient` を既存の起動パスに統合。Open Windows に read-only 表示。

#### 変更ファイル

| # | ファイル | 変更内容 |
|---|------|--------|
| 1 | `orchestrator.rs` | `build_launch_session` 直後に `SessionRegistry::register_session` |
| 2 | `window/orchestrator.rs` | `open_ready_capsule_window` 内で `attach_client(AtoWindow)` |
| 3 | `window/app_capsule_shell.rs` | `process_pending_result` で `update_process_state`。Drop の `stop_guest_session` は **まだ削除しない**（削除は D4 で実施）。Drop 停止箇所に `// TODO(D4): remove after stop_session_once wired` コメント + `tracing` を追加 |
| 4 | `window/card_switcher.rs` | `view_entries()` を既存の `OpenContentWindows::mru_order()` とマージ合成して表示。既存の window-only データソースは併用 |
| 5 | `assets/system/ato-windows/index.html` | `SessionViewEntry` 構造の表示対応。`attached_clients` 数表示 |

#### 注意点

- `process_pending_result` の Failed 更新は必ず `session_id` をキーにする
- session_id 未確定の失敗は `AppCapsuleShell::boot_state = Failed` に留め、SessionRegistry には登録しない
- **`AppCapsuleShell::Drop` の `stop_guest_session` はまだ削除しない**。既存の Drop 停止経路に `// TODO(D4): remove after stop_session_once wired` コメントと `tracing::info!("Drop: session will be stopped via legacy path (to be removed in D4)")` を追加する
- D2b 完了時点では、window close でセッションは停止する。Detached 状態は D4 で有効化

### PR-D3: 起動リクエスト + モーダル分離

**目的**: `PendingLaunches` を launch_id Map 化。os-browser でも E103/E302 を通す。
`prepare_launch_session` と `attach_session_client` を分離。

#### 変更ファイル

| # | ファイル | 変更内容 |
|---|------|--------|
| 1 | `state/session.rs` | `PendingLaunches`, `LaunchRequestId`, `CapsuleLaunchRequest` 定義 + GPUI global 登録 |
| 2 | `window/launch_window.rs` | `PendingLaunchTarget(Option<GuestRoute>)` → 削除。`open_consent_window_for_route(cx, request: CapsuleLaunchRequest)` に変更 |
| 3 | `system_capsule/ato_launch/mod.rs` | `dispatch(Approve)` が `launch_id` を受け取り `PendingLaunches` から取得 |
| 4 | `app.rs` | `NavigateToUrl` ハンドラ: os-browser でも `open_consent_window_for_route` を呼ぶ。現在の direct `resolve_and_start_guest` パスを削除 |
| 5 | `orchestrator.rs` または新規 `launch.rs` | `prepare_launch_session` + `attach_session_client` を分離 |

#### dispatch(Approve) の流れ

```
1. pending.launches から launch_id を search (remove しない)
2. 状態を ApprovedStarting に更新
3. prepare_launch_session 実行
4. 成功 → launches.remove + attach_session_client
5. E103/E302 → 状態を BlockedAgain に更新 + モーダル再表示（request 消失しない）
6. Cancel → launches.remove
```

#### attach_session_client の分岐

```
AtoWindow / WebViewPane → open_ready_capsule_window + attach_client(AtoWindow)
OsBrowser → open_external_url(local_url) 成功後 → attach_client(OsBrowser, External)
           失敗時 → Detached 状態で登録 + diagnostic log
Headless → attach_client(Headless) のみ（UI 操作は次 PR）
```

### PR-D4: クローズ動作

**目的**: `Drop` ではなく window close event を正にする。`keep-session-running` で Detached 化、
`stop-session` で dedupe して一度だけ stop。Detached session を止める最小の StopSession 手段も
同時に提供する（D5 の全アクションを待たない）。

#### 変更ファイル

| # | ファイル | 変更内容 |
|---|------|--------|
| 1 | `app.rs` | `on_window_closed` に close behavior 分岐を追加 |
| 2 | `window/app_capsule_shell.rs` | `Drop` から `stop_guest_session` 呼び出しを削除（D2b で追加した TODO コメントを解決） |
| 3 | `window/focus_dispatcher.rs` | `CloseAppWindow` に close behavior 対応 |
| 4 | `system_capsule/ato_windows/mod.rs` | `CloseWindow` に close behavior 対応。**`StopSession` コマンドを追加**（最小 API） |
| 5 | `state/session.rs` | `detach_client` + `stop_session_once` 実装 |
| 6 | `assets/system/ato-windows/index.html` | Detached/Visible/External 行に **Stop ボタンを追加**（最小 UI） |

#### on_window_closed の処理

```
closed_window_id
→ clients_by_window_id(window_id) → Vec<SessionClientId>
→ 各 client を detach_client
→ stop-session の場合:
   session_ids = dedupe(session_id_for_client で集約)
   各 session_id に対して stop_session_once()
```

#### stop_session_once の内部実装

```
1. process_state → Stopping（二重停止防止: Stopping/Stopped の場合は no-op）
2. std::thread::spawn { stop_guest_session(sid) }
3. スレッド内完了後、AsyncApp::update() で UI thread に戻る
4. UI thread 上で registry.process_state を Stopped に更新
5. on_complete コールバック呼び出し
```

#### Drop の制限

- `AppCapsuleShell::Drop` は `detach_client` のみ
- 保険として `tracing::warn!("session may be orphaned: {sid}")` を出力
- process stop は明示イベント（`on_window_closed`、`StopSession` IPC、`StopActiveSession` アクション）のみで実行

### PR-D5: Open Windows アクション（MVP）

**目的**: セッションタイプごとの管理アクションを Card Switcher に追加。

#### 変更ファイル

| # | ファイル | 変更内容 |
|---|------|--------|
| 1 | `system_capsule/ato_windows/mod.rs` | `StopSession`, `OpenInAtoWindow`, `OpenInOsBrowser`, `CopySessionUrl`, `ShowSessionLogs`, `RestartSession` IPC コマンド追加 |
| 2 | `assets/system/ato-windows/index.html` | セッション行にアクションボタン追加（`presentation_state` に応じて出し分け） |
| 3 | `system_capsule/manifest.rs` | allowlist に新規 capability 追加 |

#### MVP アクションセット

| 表示状態 | アクション |
|----------|-----------|
| **Visible** (Ato window) | Focus、Open in Browser、Stop |
| **External** (OS browser) | Open in Ato Window、Copy URL、Stop |
| **Detached** (window closed, running) | Open in Ato Window、Open in Browser、Stop |
| **Failed** | Show Logs、Restart |
| **Stopped** | Restart、Show Logs |
| **Headless** | 表示のみ（アクションなし、次 PR 対応） |

#### セキュリティ制約

- `CopySessionUrl` は loopback address のみ許可: `url::Url::parse` 後に host が
  `127.0.0.1` / `localhost` / `::1` のいずれかであることを確認する
- `OpenInOsBrowser` も同様に loopback 判定を必須とする
- 以下の URL は拒否:
  - `0.0.0.0` / LAN IP / 外部ホスト
  - `file://...` / `javascript:...` / その他非 http スキーム
- 文字列 prefix 比較ではなく、URL parse 後の host 判定で行う

## 実装順序

```
PR-D1  →  config only（動作変更なし）
PR-D2a →  SessionRegistry skeleton（型 + unit test、統合なし）
PR-D2b →  visible window registration + Open Windows read-only 表示
          （Drop の stop_guest_session はまだ削除しない）
PR-D3  →  launch_id Map + modal decoupling + prepare/attach 分離
          （CapsuleLaunchRequest に target / origin_window_id 追加）
PR-D4  →  close behavior + 最小 StopSession UI/API
          （Drop 停止削除 + on_window_closed 分岐 + stop_session_once）
PR-D5  →  Open Windows actions full MVP
          （Open in Ato Window / Open in Browser / Copy URL / Show Logs / Restart）
```

各 PR は独立してロールバック可能。
D2b 完了時点では、window close でセッションは停止する（従来通り）。
Detached 状態の有効化と Drop 停止削除は D4 で実施する。

## 受け入れ条件

### 基本シナリオ

```
1. capsuleOpenMode=window
   - 起動モーダルが必要なら表示される
   - Ato window/pane で開く
   - window close でも default では process は残る
   - Open Windows に Detached として表示される

2. capsuleOpenMode=os-browser
   - E103/E302 が必要なら Desktop shell の modal が先に出る
   - approve 後に session が起動する
   - ready 後に OS default browser で local URL が開く
   - Open Windows に External として表示される
   - Stop session で process / port が消える

3. headless（D5 scope 外、将来対応）
   - pane/window がなくても session registry に表示される
   - Open Windows から stop/restart/open in window は次 PR
```

### Close セマンティクス

```
4. windowCloseBehavior=stop-session
   - window close で process が止まる
   - stop_guest_session は同一 session に対して一度だけ呼ばれる
   - 同一 window に複数 pane/session がある場合、全 session を dedupe して stop

5. windowCloseBehavior=keep-session-running
   - window close では process が止まらない
   - Drop だけでは process lifecycle を変更しない
   - Open Windows から明示 stop できる
   - 同一 session が別の client（OS browser）で表示されていれば、そちらは External として残る
```

### 複数 client / 同時起動

```
6. 複数 client サポート
   - 同一 session を Ato Window と OS Browser の両方に attach できる
   - Ato Window だけ閉じても OS Browser session は External として残る
   - Open Windows は 1 session = 1 row で、attached_clients 数表示

7. 複数同時起動
   - E103/E302 承認待ちが複数あっても PendingLaunch が上書きされない
   - approve 後の再 E103/E302 でも request 消失しない（BlockedAgain 状態）
```

### 終了・停止

```
8. Desktop 終了時の挙動
   - window close とは別扱い
   - app_quit_behavior は今回実装しないが、混同しない設計

9. 明示的 StopActiveSession / StopAllRetainedSessions
   - 引き続き session を停止する（Drop を経由しない）
   - Open Windows の Stop と同じ stop_session_once を経由
```

## リスクと注意点

1. **`AppCapsuleShell::Drop` からの stop 削除**
   `on_window_closed` が発火しない異常系（GPUI entity 再構築など）での
   セッションリークに注意。Drop には最低限の保険ログを入れる。

2. **`stop_session_once` の race condition**
   `Stopping` 遷移と実際の `stop_guest_session` 呼び出しの間で、
   別の close が来る可能性。`Stopping/Stopped` の場合は no-op。

3. **`LaunchRequestId` の発番**
   `AtomicU64` でスレッドセーフに発番。

4. **`launch_via` フィールド**
   CLI 起動 (`ato app session start` 手動) と Desktop 起動を区別。
   CLI 起動 session は Desktop 管理外なので、restart/stop の挙動を変える必要あり。

5. **`CopySessionUrl` のセキュリティ**
   `127.0.0.1` の local URL のみ許可。外部 URL / 未検証 URL を
   broker 経由で copy/open できると攻撃面が増える。
