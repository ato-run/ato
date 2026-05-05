# ato で何が動かせるのか: 実行境界メモ

> 作成: 2026-05-02
> 目的: 「ato で何が動くのか」と「ato-desktop でどう見せられるのか」の境界を、現行 spec と実装断片に基づいて整理する。

---

## 1. 結論

先に結論を書くと、ato の境界は **「実行できるか」** と **「ato-desktop が pane としてホストできるか」** を分けて考える必要がある。

- ato は capsule を実行するランタイム基盤であり、対象を最初から Web アプリだけに限定していない。
- ただし ato-desktop は最終的に session envelope の `display_strategy` を見て表示方法を決める。
- そのため「動く」と「ato-desktop の Wry WebView pane に自然に載る」は同義ではない。

根拠:

- [docs/rfcs/accepted/CAPSULE_HANDLE_SPEC.md](../docs/rfcs/accepted/CAPSULE_HANDLE_SPEC.md)
- [docs/rfcs/accepted/ATO_CLI_SPEC.md](../docs/rfcs/accepted/ATO_CLI_SPEC.md)

---

## 2. 境界の見方

ato 側の判断軸は、アプリ種別そのものではなく session contract の `display_strategy` にある。

現行 spec で少なくとも定義されている表示戦略は次の 5 つ。

- `guest_webview`
- `web_url`
- `terminal_stream`
- `service_background`
- `unsupported`

つまり「Electron だから不可」「Go 製 Web アプリだから可」という雑な線引きではなく、最終的にはその capsule がどの表示戦略に落ちるかで ato-desktop 側の UX が決まる。

根拠:

- [docs/rfcs/accepted/CAPSULE_HANDLE_SPEC.md](../docs/rfcs/accepted/CAPSULE_HANDLE_SPEC.md)
- [docs/rfcs/accepted/ATO_CLI_SPEC.md](../docs/rfcs/accepted/ATO_CLI_SPEC.md)

---

## 3. いま確実に言える境界

### 3.1 そのまま載せやすいもの

#### A. `runtime=web` で `local_url` に attach できるもの

これは ato-desktop から見ると最も素直な経路。CLI が起動した Web surface に対して Desktop が `local_url` で attach する。

- 典型例: localhost で HTTP を待ち受ける Web アプリ
- `metadata.ato_desktop_guest` は必須ではない
- ato-desktop では基本的に `web_url` として扱われる

根拠:

- [docs/rfcs/accepted/ATO_CLI_SPEC.md](../docs/rfcs/accepted/ATO_CLI_SPEC.md)

#### B. guest contract に載ったフロントエンド

これは「アプリの Web フロントを host 側 WebView に載せ、必要な host API だけを shim で注入する」経路。ato-desktop に自然に統合しやすいのはむしろこちら。

- `metadata.ato_desktop_guest` で guest frontend を宣言する
- host 側が adapter ごとの bridge を提供する
- Desktop では `guest_webview` として扱える

この workspace には少なくとも次の adapter サンプルがある。

- Tauri guest: [samples/desky-mock-tauri/capsule.toml](../samples/desky-mock-tauri/capsule.toml)
- Wails guest: [samples/desky-mock-wails/capsule.toml](../samples/desky-mock-wails/capsule.toml)
- Electron guest: [samples/desky-mock-electron/capsule.toml](../samples/desky-mock-electron/capsule.toml)

Tauri 互換 API の preload 実装も repo 内に存在する。

- [crates/ato-desktop/assets/preload/tauri.js](../crates/ato-desktop/assets/preload/tauri.js)

guest contract 上で現在明示されている互換面は次の通り。

- Tauri: `window.__TAURI_INTERNALS__.invoke`, `window.__TAURI__.fs`, `window.__TAURI__.dialog`, `window.__TAURI__.window`, `window.__TAURI__.shell`
- Wails: `window.runtime.Invoke`, `window.runtime.invoke`, `window.go.main.App.*`
- Electron guest: limited `window.electron.ipcRenderer.invoke` allowlist

根拠:

- [docs/rfcs/archived/DESKY_GUEST_CONTRACT.md](../docs/rfcs/archived/DESKY_GUEST_CONTRACT.md)

### 3.2 動かせても pane に載せにくいもの

#### C. native shell 依存が強いデスクトップアプリ

ここで言う「依存が強い」は、アプリ価値の中心が Web UI ではなく host 側 shell 機能に強く結びついている状態を指す。

たとえば次の要素が強いと、ato による guest 化コストが上がる。

- 自前の `BrowserWindow` や native window 管理が本質機能
- preload 経由の独自 IPC が大量にある
- unrestricted な Electron main process 権限を前提にしている
- tray, native menu, global shortcut, OS API 呼び出しが本質機能になっている
- host から提供される少数の allowlist API では成立しない

重要なのは、これは「実行できない」という意味ではなく、**無改造のまま guest_webview に落としにくい**という意味であること。

つまり境界はこうなる。

- Electron/Tauri 製でも guest contract に寄せられるなら ato-desktop に統合できる
- Electron/Tauri 製でも native shell 依存が強いままだと pane 統合の優先度は下がる

---

## 4. よくある誤解と修正

### 誤解 1: Electron アプリは ato で動かせない

これは誤り。少なくともこの repo は Electron guest adapter を想定している。

- [samples/desky-mock-electron/capsule.toml](../samples/desky-mock-electron/capsule.toml)
- [samples/desky-mock-electron/backend/server.py](../samples/desky-mock-electron/backend/server.py)

正確には、「Electron 本体をそのまま起動する経路」と「Electron 互換 frontend 契約を host 側 Wry で再現する経路」を区別する必要がある。

### 誤解 2: Web サーバ型アプリだけが capsule 化対象

これも言い過ぎ。Web サーバ型は最短経路だが、それだけではない。

- `runtime=web` の `web_url`
- guest contract の `guest_webview`
- terminal/service 系の `terminal_stream` / `service_background`

という複数の着地点がある。

### 誤解 3: guest 化できれば任意の native API がそのまま使える

これも誤り。現状の guest contract は互換面をかなり絞っている。

- Electron guest は limited allowlist
- Tauri/Wails も互換 preload 経由
- 権限昇格や UI mode 変更は Host Bridge と consent flow の管理下にある

根拠:

- [docs/rfcs/archived/DESKY_GUEST_CONTRACT.md](../docs/rfcs/archived/DESKY_GUEST_CONTRACT.md)
- [docs/TODO.md](../docs/TODO.md)

---

## 5. 実用上の判断基準

ato-desktop で日常利用する capsule 候補を選ぶときは、実装言語よりも次を見た方がよい。

1. `display_strategy` を何に落とせるか
2. guest contract に寄せる必要があるなら、その IPC 面は小さいか
3. データ永続化が単純か
4. host 側 shell に window 管理を明け渡せるか
5. standalone app と guest mode の二重起動モデルを整理できるか

この観点では、次の順に扱いやすい。

1. `web_url` に素直に落ちる Web アプリ
2. `guest_webview` に載せやすい guest-aware frontend
3. `terminal_stream` や `service_background` で十分なツール
4. shell 依存が濃い無改造デスクトップアプリ

---

## 6. 今日時点の境界まとめ

短く言い切るなら次の通り。

- ato は Web アプリ専用基盤ではない
- ato-desktop は `display_strategy` ベースで表示を決める
- `runtime=web` の `local_url` attach は現時点で最も素直
- Tauri/Wails/Electron 互換の guest frontend は、条件付きで host 側 Wry pane に統合可能
- ただし guest 契約に乗らない native shell 依存の強いアプリは、無改造のまま pane に自然統合するには向かない
- したがって「何が動くか」の境界は、アプリカテゴリではなく **表示戦略と guest 契約に乗るかどうか** で決まる

---

## 7. 未完了の部分

この境界メモは「将来こうしたい」ではなく「今日 repo から確認できる範囲」に寄せている。ただし guest mode 周辺はまだ実装継続中で、少なくとも次が TODO に残っている。

- Playwright による JSON-RPC round-trip テスト
- consent dialog の E2E
- headless mode の sidebar indicator
- guest が env を正しく受け取るテスト

根拠:

- [docs/TODO.md](../docs/TODO.md)