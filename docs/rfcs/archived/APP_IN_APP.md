# App-in-App / Guest Mode Specification (Draft)

**Status:** v0.3 (Ato/Magnetic Web 対応版)  
**最終更新:** 2026-02-01

## 1. 目的
- ホストアプリ（Ato Runtime）内での安全かつ流体的なカプセル実行
- **Single Guest Context**: UIモードに依存しない統一された機能呼び出し（Capability）
- ユーザーの意図に応じた動的なモード遷移（Fluid Transition）

## 2. ロール
- **Host (Ato Runtime)**: コンテナ管理、権限移譲、ライフサイクル管理を行う。
- **Guest (Capsule)**: 定義されたCapabilityとUIを提供する。

### 2.1 Context
- **Consumer Context**: `payload` を Read-Only で利用。
- **Owner Context**: 書き戻し（Write-back）権限を持つ。

## 3. 権限とCapability
Magnetic Webにおいて、UIは「表面（Surface）」であり、本質は「能力（Capability）」にある。

### 3.1 Capability Definition (MCP-like)
カプセルは `capsule.toml` および `capabilities.json` で自身の機能を定義する。

- **Schema**: ホストが動的にAPIを生成するための型定義（関数名、引数、戻り値）。
- **Independence**: UI（Widget/App）がなくても、ホスト（AIエージェント）はこのスキーマを通じて機能を直接実行できる。

### 3.2 権限委譲
- **原則**: Guestの有効権限 = `Host委譲` ∩ `Guest Manifest (allowlist)`
- **Mode Transparency**: UIモード（Headless/Widget/App）によって権限スコープが自動的に変わることはない。
    - 例外: ユーザーがファイルダイアログ等で明示的に指定した場合（User Intent）のみ、一時的なアクセス権が付与される。

## 4. 起動モード (Display Context)
ホストはカプセルを以下のいずれかのコンテナで起動する。

| モード | UIメタファー | 振る舞い | ユースケース |
| :--- | :--- | :--- | :--- |
| **Headless** | **Status Icon** | **不可視 / 常駐アイコン**<br>UIを描画せず、Capabilityの実行のみを受け付ける。トレイアイコン等でステータス表示は可能。 | バックグラウンド処理<br>通知<br>AIによるAPI利用 |
| **Widget** | **PiP (Floating)** | **HTML Overlay / Picture-in-Picture**<br>ホスト画面の最前面に浮遊する独立ウィンドウ。ユーザーが自由に移動可能。ページ遷移しても常駐する。 | 計算機<br>AIチャット<br>常時監視パネル |
| **App** | **Tab / Window** | **Immersive / Workspace**<br>メインビューを占有、または新規タブとして展開。深い没入作業を行う。 | ダッシュボード<br>エディタ<br>複雑な設定 |

## 5. 通信プロトコル (Unified Guest Protocol)
**JSON over stdio** (1 request / 1 response) を用いる。
通信内容はモードに依存せず統一される。

### 5.1 環境変数 (Host → Guest)
- `CAPSULE_GUEST_PROTOCOL`: `guest.v2`
- `GUEST_INITIAL_MODE`: `headless` | `widget` | `app`
- `GUEST_ROLE`: `consumer` | `owner`
- `SYNC_PATH`: 対象 .sync のパス
- `ALLOW_HOSTS`: 許可されたホスト一覧
- `ALLOW_ENV`: 許可された環境変数一覧

### 5.2 標準メッセージフォーマット (JSON-RPC)
```json
{
    "version": "guest.v2",
    "id": "req_123",
    "action": "InvokeCapability", 
    "payload": {
        "capability": "calculate_tax",
        "params": {
            "price": 1000,
            "rate": 0.1
        }
    }
}

```

### 5.3 Core Actions

Host-Guest間で交わされる基本アクション。

#### Capability / Data Access

* `InvokeCapability(name, params)`: 定義された関数の実行。
* `ReadPayload() / WritePayload(data)`: .syncデータの読み書き。
* `ReadContext() / WriteContext(json)`: コンテキストの読み書き。

#### Lifecycle / Layout (Negotiation)

* **`RequestResize(width, height)`**: (Widget/App) コンテンツサイズに応じたリクエスト。
* **`RequestModeChange(target_mode)`**: GuestからHostへのモード遷移要求（例: 「詳細を見る」ボタンでAppモードへ）。
* `Terminate()`: 終了要求。

## 6. UIレイアウトとネゴシエーション

Widgetモードは「埋め込み」から「独立フローティング」に変更されたため、座標指定はHost主導の初期配置と、User主導の移動に委ねられる。

### 6.1 静的定義 (`capsule.toml`)

起動前のプレースホルダーや初期ウィンドウ生成に使用。

```toml
[ui.widget]
default_width = 300
default_height = 400
min_width = 200
resizable = true
default_placement = "top-right" # center | top-right | pointer

```

### 6.2 動的リサイズ

1. Guestはコンテンツ描画後、必要サイズを `RequestResize` で送信。
2. Hostは画面制約（Available Area）と照らし合わせ、許可または制限付きで反映。

## 7. セキュリティ

* **Sandbox**: 全モードで適用。OSレベルの隔離（Landlock/Seatbelt）。
* **Egress Control**: `ALLOW_HOSTS` に基づく厳格なフィルタリング。Host側でプロキシ/Sidecarによる強制適用を推奨。

## 8. 未決事項

* Headlessモード時のアイコン/バッジ更新プロトコル詳細
* マルチウィンドウ（OS Window）への昇格/デタッチの仕様
