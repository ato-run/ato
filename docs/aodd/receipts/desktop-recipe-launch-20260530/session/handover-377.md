# #377 実装進捗と現状 — Handover Document

**Date:** 2026-05-31  JST  
**Branch:** `dev` (1305eba7)  
**Platform:** Windows 11 x86_64  

---

## ✅ 解決済み (2026-05-31 追記) — Desktop orchestrator ハングの根本原因

下記「現在の状態」で未解決だった **「Container は起動するが WebView pane が
出現しない」** の真因を特定・修正し、E2E で確認した。

### 真因
Desktop は `ato app session start … --json` を `Command::output()` で起動する。
`output()` は **子プロセス終了 AND stdout/stderr パイプの EOF（全 write ハンドル
閉鎖）** の両方を待つ。OCI/web カプセルでは orchestrator が
`<engine> logs --follow <container>`（`oci_provider.rs::logs()`）という**長命な
子プロセス**を spawn してコンテナログを stderr にミラーする。Windows ではこの子が
`bInheritHandles=TRUE` で **親 `ato` の stdout/stderr パイプ write ハンドルを継承**
し、`--follow` でコンテナ停止まで生き続けるため、`ato app session start` 本体が
exit 0（エンベロープ出力済み）で終了しても **パイプが閉じず `output()` が永久
ブロック**。結果、launch スレッドは "resolving capsule" で停止し WebView pane が
作られなかった。`app session watch-parent` ウォッチャーも同様に継承する第二の子。
POSIX は `output()` のパイプ fd が `CLOEXEC` のため発生しない。

→ 当初診断（source build path / podman DNS / Focus dispatcher が pane を作らない）
は **すべて誤り**。CLI エンベロープは常に正しく生成されており、Desktop への
**受け渡し**だけがブロックされていた。

### 修正
`crates/ato-cli/src/app_control/session.rs` `start_session`：JSON エンベロープ
モード（= Desktop 経路）で、orchestration が子を spawn する**前**に、自プロセスの
stdout/stderr の `HANDLE_FLAG_INHERIT` をクリア（Windows のみ、`windows-sys`）。
以降どの子もパイプを掴まないため、session start 終了と同時に EOF が立ち `output()`
が即返る。`Cargo.toml` に `windows-sys`(Win32_Foundation, Win32_System_Console)追加。

### 検証
- 隔離 repro（piped `output()`）：session-start exit の **0.3 秒後**に両パイプ EOF。
- Desktop E2E（pgweb）：`browser_tabs` が live URL の `guest-capsule` pane を返し、
  `browser_take_screenshot` が PNG を返す。ログは ~8 秒で
  resolving → capsule session started → readiness passed → AppWindow opened まで完走。
- レシート：`pgweb/receipt.md` を **complete** に更新。

---

## 完了したこと

### 1. 原因特定

Desktop が `capsule://github.com/usememos/memos` のような URL を受け取った時の流れは以下の通り：

```
Desktop NavigateToUrl
→ focus_dispatcher (app.rs)
→ launch_window::open_consent_window_for_route
→ ForceApprovePending
→ start_boot_launch
  → スレッド spawn: resolve_and_start_guest_with_input
    → resolve_and_start_capsule("github.com/sosedoff/pgweb", ...)
      → resolve_capsule → "ato app resolve github.com/sosedoff/pgweb --json"  ✅ 正常
      → start_capsule → "ato app session start github.com/sosedoff/pgweb --json"  ✅ 正常
      → build_launch_session → CapsuleLaunchSession 構築
  → フォアグラウンド poll: rx.try_recv()
  → Ok(session) 受信
  → open_ready_capsule_window
    → AppCapsuleShell::new_ready
    → FocusGuestPaneRegistry::register
    → WebView 作成
```

**Key insight**: `resolve_session_launch_plan` (session.rs:1863) は **すでに正しく sample recipe を検出している**。`capsule://github.com/usememos/memos` も `github.com/usememos/memos` も、`normalize_capsule_handle` → `resolve_sample_recipe_for_github` → OCI manifest のパスを通る。ソースビルド path には落ちない。

**つまり #377 の「source build path に入ってしまう」という当初の診断は、実は正しくなかった。真の原因は異なる。**

### 2. 実装した変更

#### ✅ `sample_recipes.rs` に pgweb + adminer を追加

**File:** `crates/ato-cli/src/app_control/sample_recipes.rs:79-92`

```rust
SampleRecipeBinding {
    slug: "pgweb",
    display_name: "pgweb",
    aliases: &["pgweb"],
    github: Some(("sosedoff", "pgweb")),
    manifest_content: include_str!("../../../../samples/recipes/pgweb/capsule.toml"),
},
SampleRecipeBinding {
    slug: "adminer",
    display_name: "Adminer",
    aliases: &["adminer"],
    github: Some(("vrana", "adminer")),
    manifest_content: include_str!("../../../../samples/recipes/adminer/capsule.toml"),
},
```

#### ✅ mount path テスト追加 (`manifest_validation.rs`)

- `recipe_state_binding_paths_accepted_on_windows` — 11 recipe paths が受け入れられること
- `relative_container_target_data_is_rejected` — 相対パス・`./data` が拒否されること

---

## 現在の状態

### CLI: 完全動作

```
# すべて成功
ato run --plan-only samples/recipes/memos --yes                   ✅
ato app session start capsule://github.com/sosedoff/pgweb --json   ✅ (exit 0, container starts)
ato app session start github.com/sosedoff/pgweb --json             ✅ (exit 0, container starts)
ato run samples/recipes/memos --yes --state data=C:\...\memos      ✅ (stateful OCI via Docker Desktop)
```

### Desktop: Container は起動するが WebView pane が出現しない

| チェック項目 | 結果 |
|-------------|------|
| sample recipe 解決 | ✅ 正常（log に "preflight failed: manifest path does not exist" が出なくなった） |
| コンテナ起動 | ✅ `docker ps` で `ato-pgweb-*-main` が Up、`http://127.0.0.1:XXXXX/` で応答あり |
| AppCapsuleShell 作成 | ❓ 未確認 |
| FocusGuestPaneRegistry 登録 | ❌ 未確認 |
| `browser_tabs` に guest-capsule 出現 | ❌ `{"panes":[]}` |
| WebView 描画 | ❌ "no WebView pane" |

### 疑わしい点

Desktop log が `"resolving capsule handle"` で止まり、その後の `"capsule session started"` や `"AppWindow opened"` が出ない。しかしコンテナは起動している。
→ `resolve_and_start_capsule` が `start_capsule` の後で戻ってこない可能性がある。

調査候補：
1. `SurfaceStageTimer` / `on_step` callback の channel でブロック
2. `build_launch_session` が失敗
3. `WebviewInitGuard::is_active()` が true のまま → フォアグラウンド poll がスキップ
4. スレッド間通信の問題
5. `ato_helper_command` が設定する環境変数（`ATO_HOME` 等）の違い

---

## 残作業

### 優先度 高

1. **Desktop orchestrator のハング原因特定**
   - `resolve_and_start_capsule` の各ステップに `tracing::info!` を追加してボトルネックを特定
   - `build_launch_session` が失敗しているのか、それとも `start_capsule` が返ってこないのか
   - `WebviewInitGuard` の状態確認

2. **WebView pane 出現の確認**
   - 上記ハングを解消すれば、`open_ready_capsule_window` → `AppCapsuleShell::new_ready` → `FocusGuestPaneRegistry::register` の流れで pane が登録されるはず
   - `browser_tabs` が `guest-capsule` を返すこと
   - `browser_take_screenshot` が PNG を返すこと

### 優先度 中

3. **excalidraw image fix**
   - 現在 `exec container process /docker-entrypoint.sh: Exec format error` で起動不可
   - recipe の `cmd = ["sh", "-c", "sed ..."]` が Linux 依存
   - Windows 用に cmd override を削除するか、互換性のある entrypoint を使う

4. **Fix 3: platform-aware source build error（#377 の一部）**
   - 未知の GitHub source が Windows で `/bin/sh` を要求する場合の typed error
   - 実装場所: smoke test 実行前、または `resolve_run_target_or_install` 内

### 優先度 低

5. **残りの recipe を catalog に追加**
   - pocketbase, homepage, linkwarden, langflow, shiori, filebrowser

---

## ファイル変更一覧

| File | Change |
|------|--------|
| `crates/capsule/src/config/config_impl.rs` | `container_runtime` field added to `CapsuleConfig` |
| `crates/capsule/src/engine/executors/oci.rs` | `detect_engine()` → health-check based selection |
| `crates/capsule/src/packers/oci.rs` | `detect_engine()` → health-check based selection (mirror) |
| `crates/capsule/src/adapters/capsule/cas_store.rs` | `use std::path::PathBuf` added (Windows build fix) |
| `crates/capsule/src/packers/payload.rs` | `#[cfg(unix)]` guards for Unix imports (Windows build fix) |
| `crates/capsule/src/foundation/types/manifest_validation.rs` | Mount path tests added |
| `crates/ato-cli/src/app_control/sample_recipes.rs` | pgweb + adminer added to catalog |

---

## GitHub Issues 状態

| Issue | Status |
|-------|--------|
| #370 | ✅ Closed — Focus guest WebView (PR #374 + #375) |
| #372 | ✅ Closed — State binding path validation |
| #373 | ✅ Closed — Docker Desktop runtime selection |
| #377 | 🟡 Open — 本 issue。CLI 動作確認済、Desktop orch ハング未解決 |
| #369 | 🔵 Open — Windows AODD (0 PASS, 16 BLOCKED, pending #377) |

---

## 再現手順（次作業者向け）

```powershell
# 1. ビルド
cd C:\Users\koh\ato
cargo build -p ato-cli --bin ato
cd crates\ato-desktop && cargo build --bin ato-desktop

# 2. クリーン
Get-Process -Name "ato-desktop","ato" -ErrorAction SilentlyContinue | Stop-Process -Force
docker ps -q | ForEach-Object { docker stop $_ }
Remove-Item -Force "C:\Users\koh\.ato\run\*" -ErrorAction SilentlyContinue

# 3. Desktop 起動
$env:ATO_DESKTOP_ATO_BIN = "C:\Users\koh\ato\target\debug\ato.exe"
Start-Process "C:\Users\koh\ato\crates\ato-desktop\target\debug\ato-desktop.exe"

# 4. MCP テスト
python docs\aodd\mcp_client.py  # 適宜修正
# → host_dispatch_action NavigateToUrl capsule://github.com/sosedoff/pgweb
# → ForceApprovePending
# → Wait → browser_tabs → browser_take_screenshot

# 5. ログ確認
Get-Content "C:\Users\koh\.ato\logs\ato-desktop.log.2026-05-31" -Tail 30
```
