# 📄 Desktop Surface Materialization Specification

**Document ID:** `SURFACE_MATERIALIZATION`
**Status:** Draft v0.1
**Target:** ato-desktop v0.5.x
**Last Updated:** 2026-04-29

> **Scope.** This RFC covers only the latency between the user clicking a
> capsule launcher in `ato-desktop` and the first interactive paint of the
> capsule's UI. It does **not** redefine the WebView security model, the
> `capsule<partitionId>://` protocol, or the bridge / IPC contracts —
> those are taken as-is from the existing implementation.

## 1. 概要 (Overview)

`ato-desktop` の click → 表示 までを「毎回 fresh process + fresh WebView を
組み立てる step」から「declared launch contract から導出される **Desktop
Surface** が local desktop state 上で materialized 済みかを確認し、missing
/ stale のときだけ新しく組み立てる step」に再定義する。

[BUILD_MATERIALIZATION](BUILD_MATERIALIZATION.md) と
[APP_SESSION_MATERIALIZATION](APP_SESSION_MATERIALIZATION.md) と同型の拡張:

```
Build:        inputs + command + toolchain   →  build artifact
                                              →  .ato/state/materializations.json

App Session:  launch spec + readiness         →  running process + bound port
                                              →  ~/.ato/apps/<pkg>/sessions/<id>.json

Surface:      session record + UI contract    →  ready WebView + first paint
                                              →  in-process WebView pool / pane state
```

### 1.1 解決する問題 — PR 2 後の baseline

PR `1a33a89` (App Session Materialization v0) によって `ato app session start`
の warm 経路は 4ms に短縮された。ところが Desktop の click → 表示 体感は依然
**~3.5 秒**残っている。ユーザ報告と内訳推定:

```
Desktop click
  ├─ ato CLI subprocess spawn + envelope return:    ~10 ms (warm reuse)
  ├─ ato CLI 自体の cold-start (binary load, clap):  ~150 ms ★ (1)
  ├─ orchestrator が envelope を parse / pane state 構築:  ~50 ms
  ├─ WebView 生成 (Wry, WKWebView 初期化):              ~300 ms ★ (2)
  ├─ WebView が `http://127.0.0.1:<port>/` を fetch:    ~50 ms
  ├─ Next.js SSR の first / response:                    ~300 ms ★ (3)
  ├─ JS bundle download + parse + execute + hydrate:   ~1200 ms ★ (4)
  └─ first interactive paint:                            ~300 ms
                                                        ────────
                                                        ~2360 ms
```

★ がついた 4 箇所が支配的。残り 1100ms は Next.js / Wry / WKWebView の
intrinsic コストで、Ato 側からの介入余地は限定的。**重要なのは PR 1 の
build phase でやったのと同じく、まず実測でこの内訳を裏付けてから設計を
進めること**（§5 Phase 0）。

| 問題 | 具体 |
|---|---|
| 同一 capsule への click ごとに WebView を再生成 | `WebViewManager::sync_from_state`（webview.rs:376）で pane の partition_id が変わるたびに WebView を tear down + 新規生成。pool / preload なし |
| ato CLI subprocess が常に必要 | `orchestrator::resolve_and_start_capsule`（orchestrator.rs:370）が `ato app session start` を spawn。warm reuse でも fork+exec の固定コスト ~150ms |
| 初回 fetch まで surface が空 | WebView は about:blank → URL 切替 → SSR 待ち の直列。skeleton / app shell が無く体感が長い |
| URL prefetch 未実装 | session record から `local_url` が取れた瞬間に並行 fetch を開始すれば SSR を warm にできるが、現在は WebView 生成後に初めて HTTP が走る |

### 1.2 設計方針

- **Surface は phase ではなく artifact**: WebView は毎回作る UI 部品ではなく、
  capsule session に対応する materialized resource として扱う。
- **Pre-warming でしか勝てない**: Desktop click → 表示 の支配項は process /
  network / JS exec の固定コスト。最大の win は「click した瞬間にはもう
  WebView も SSR も warm」状態を準備しておくこと。
- **3 層モデルへの整合**: 宣言（`capsule.toml [surface]`）／解決結果（session
  record）／実機状態（in-process WebView pool）。
- **Measurement first**: PR 1 で build phase の sub-stage timing を入れたのと
  同じく、Phase 0 として `SURFACE-TIMING` を Desktop 側に入れて 3.5s の
  decomposition を確定させてから次の Phase に進む（§5）。
- **CLI 互換は壊さない**: `ato app session start` の subprocess 経路を絶対に
  廃止しない。Surface materialization は **既存経路の上の追加レイヤ**として
  入れる。失敗したら subprocess fallback。
- **Ato philosophy**: "Reuse the model, not special cases" — 既存の Build
  / App Session materialization と概念的に揃える。Phase 名・state 名・
  result_kind 値集合の prefix を共通化する。

### 1.3 Build / App Session Materialization との関係

| 軸 | Build (RFC) | App Session (RFC) | Surface (本 RFC) |
|---|---|---|---|
| Materialized 対象 | output tree | running process + bound port | ready WebView + first paint |
| Digest 入力 | source tree + command + toolchain | LaunchSpec + build digest + readiness | session record digest + Wry partition_id + display constraints |
| Storage | `.ato/state/materializations.json` (project-local) | `~/.ato/apps/<pkg>/sessions/` (host-local) | in-process pool + (optional) `~/.ato/apps/<pkg>/surfaces/` |
| Stale 検知 | digest mismatch + outputs missing | pid 死亡 / start_time / digest / healthcheck | session_id mismatch / WebView crashed / window closed |
| Reuse 後 result_kind | `materialized` | `materialized-session` | `materialized-surface` |
| Force 再生成 flag | `--rebuild` | `--respawn` (v1+) | TBD (v1+, §8) |
| 必須 acceptance | warm < 100ms | warm < 50ms | warm < 100ms (target、§10) |

3 つの materialization layer がそれぞれ独立して効くと、cold cycle のうち
最後の 1 回だけが本当の "cold" で、それ以降は **build skip + session reuse +
surface reuse** が直列に決まり click → 表示が体感で sub-second 域に近づく。

## 2. コアコンセプト

### 2.1 Desktop Surface

Desktop Surface は次の関数として定義される:

```
surface = surface_executor(SessionInfo, SurfaceConstraints)
```

- `SessionInfo`: APP_SESSION_MATERIALIZATION で得られる envelope（`pid` /
  `local_url` / `display_strategy` 等）
- `SurfaceConstraints`: pane 内での bounds / partition_id / preload script
  プロファイル（既存 `WebViewManager` の入力と同じ）
- 結果: `{ webview_handle, navigated_url, ready_at, partition_id }` を返す
  materialized record

### 2.2 Surface Pool

`WebViewManager` に **pre-warmed WebView の pool** を持たせる。pool 内の
WebView は:

- `about:blank` か lightweight loading shell（後述）を最初に load してある
- partition_id は **placeholder**（実際の capsule への bind は acquire 時に
  行う）
- bounds は invisible / 0-size（claim されたら正規 bounds に拡張）

acquire 時:
```
pane click
  ├─ try_acquire_from_pool(partition_id, scheme, bounds)
  ├─ if hit → load_url(local_url) のみ。WebView 生成コストを回避
  └─ if miss → 既存経路で新規 WebView 生成
```

### 2.3 Surface Prefetch

session record から `local_url` が取れた瞬間（subprocess の envelope 受領
時、または disk から直読した時）、Desktop の async runtime で **HTTP GET
を 1 回先行して打つ**。これは Next.js server の SSR cache を warm にし、
WebView が同じ URL に navigate する時の応答時間を短縮する。

prefetch は best-effort: 失敗しても表示は壊れない。

### 2.4 App Shell / Skeleton

WebView が空の状態を見せず、navigate 完了までは loading skeleton を表示
する。これは latency を物理的に削るのではなく **体感** を改善する。

実装は既存の `capsule://welcome/index.html` 流用または bundled なミニマル
loading HTML。WebView 生成直後に load_url (data:..., text/html, ...) で
即座に表示し、`local_url` への切替は session ready 後。

## 3. 実装フェーズ

このRFCは 4 つの phase に分けて段階的にロールアウトする。各 phase は
独立に shippable で、計測値で次の phase の必要性を判断する。

### Phase 0: Measurement (PR 3)

**目的**: 3.5s の decomposition を実測で裏付ける。設計の根拠を固める。

**変更**:
- Desktop 側に `SURFACE-TIMING` を追加（CLI の `PHASE-TIMING` と同形式、
  stderr 出力、`ATO_SURFACE_TIMING=1` で有効）
- 計測点（最低限）:
  - `subprocess_spawn` (`run_ato_json` 開始 → exec 戻り)
  - `envelope_parse` (orchestrator が SessionStartEnvelope を deserialize)
  - `pane_state_build` (`build_launch_session`)
  - `webview_create` (Wry `WebViewBuilder::build`)
  - `webview_load_url` (load_url 呼び出し → `did_finish_navigation`)
  - `first_meaningful_paint` (Wry コールバック / postMessage 経由で
    WebView から signal)
- byok-ai-chat で cold/warm を 5 回ずつ計測し、§1.1 の推定との差分を確認

**Out of scope**: implementation の最適化はしない。計測のみ。

**Acceptance**:
- `ATO_SURFACE_TIMING=1` で sub-stage 行が出る
- 各 stage の elapsed_ms 中央値が記録される
- §1.1 の推定値との差が 50ms 以内なら推定どおり、外れたら設計を再検討

### Phase 1: Subprocess elimination (PR 4)

**目的**: ato CLI の cold-start ~150ms を warm reuse path で消す。

**変更**:
- Desktop が `ato app session start` を spawn する前に、まず session record
  を直接 disk から読む fast path を追加
- pseudo:
  ```
  if let Some(record) = read_session_record_directly(handle, target) {
      if validate_record_for_reuse(record) {
          return SessionInfo::from(record);
      }
  }
  // fall back to subprocess
  run_ato_json(&["app", "session", "start", handle, "--json"])
  ```
- validate_record_for_reuse の **5 条件は CLI 側 v0 と完全一致**させる
  （schema_v2 / digest / pid alive / start_time / healthcheck）。実装は
  `ato-cli` のヘルパを capsule-wire 経由で再エクスポートするか、Desktop
  側に shim を切る
- digest 計算は CLI を呼んで取得 (slow path) するか、Desktop 側でも
  `LaunchSpec` を canonical 化する (fast path)。後者は CLI と Desktop 双方の
  bug fix が必要なので、v0 では **handle/target/local_url の partial match**
  で satisfy し、CLI 側の真の digest 計算は subprocess に委ねる妥協案も可

**Out of scope**:
- 失敗時 fallback: 必ず subprocess
- multi-instance / `--respawn`: CLI v1 を待つ
- daemon 化: 別 RFC

**Acceptance**:
- `ATO_SURFACE_TIMING=1` で `subprocess_spawn` stage が消える（warm reuse 時）
- byok-ai-chat warm reuse の Desktop 側 hot path が 150ms 短縮される
- session record が壊れていた / 5 条件 fail のとき subprocess fallback で
  cold path と同じ結果を出せる

### Phase 2: WebView Pool (PR 5)

**目的**: WebView 生成 ~300ms を消す。

**変更**:
- `WebViewManager` に pool を追加: GPUI app 起動時に N 個（v0 では 1〜2 個）
  の WebView を about:blank で pre-create
- pane click 時に pool から acquire → load_url で実 URL に切替
- pool が空の場合は既存経路で新規生成（fallback）
- 解放時の挙動: pane 閉じ時に WebView を destroy するのではなく、可能なら
  about:blank に戻して pool に戻す

**設計上の難所**:
- partition_id（`capsule<id>://` scheme）は WebView 生成時に scheme handler
  と紐付くため、実 capsule への bind は pool acquire 時にやり直す必要がある。
  この re-bind が安全かを Wry / WKWebView API で検証する必要がある（pool
  実装時の最大の risk）
- preload script (host_bridge.js + adapter shims) はプロファイルが capsule
  ごとに違う。pool 内 WebView は generic profile で起動し、acquire 時に
  追加の bridge config を inject する形が現実的

**Out of scope**:
- pool size の dynamic 調整（idle 時に縮小、突発負荷で拡大）: v1
- multi-window / multi-monitor 配慮: v1+

**Acceptance**:
- `ATO_SURFACE_TIMING=1` で `webview_create` stage が pool hit 時に < 10ms
- byok-ai-chat warm の Desktop hot path が累計 300ms 短縮される
- partition_id 切替後に bridge / preload が壊れていない（既存の bridge
  unit test を pass、加えて手動 e2e）

### Phase 3: Surface Prefetch + App Shell (PR 6)

**目的**: SSR + JS bundle の体感コストを ~500ms 削る。

**変更**:
- session record / envelope から `local_url` が取れた直後に async fetch を
  発火（`tokio::spawn` で `reqwest::get(local_url)` を fire-and-forget）
- WebView は pool acquire 時に bundled loading skeleton（`capsule://__loading__/`
  scheme でホストする static HTML）を即表示
- `did_finish_navigation` が `local_url` 側で fire したら skeleton から実
  page に切替（user 視点では skeleton → 実 UI のクロスフェード）

**設計上の難所**:
- prefetch のセキュリティ: capsule の per-pane fetch は `host_bridge` の
  capability allowlist で制御されるが、prefetch は host process 側からの
  fetch なので allowlist の対象外。ただし対象 URL は capsule 自身が宣言した
  `local_url` のみで、外部送信を含まないため許容
- skeleton と実 page のクロスフェード: WKWebView では `load_url` の途中で
  前の document が見え続けるため、**「先に空白 → 切替で空白からの遷移を
  消す」**が達成しづらい。data: URI の skeleton を上書きする形でも単に
  flash するだけで、本質的に first-contentful-paint は早くならない可能性
  あり

**Out of scope**:
- WebView 内での bundle prefetch（service worker 経由）: out of scope
- offline-first 対応: 別 RFC
- HTTP/2 push 等の transport 最適化: 別 RFC

**Acceptance**:
- `ATO_SURFACE_TIMING=1` で `prefetch_started` / `prefetch_completed`
  stage が出る
- byok-ai-chat warm の `webview_load_url` stage が 200ms 以上短縮される
- skeleton 表示で「click → 何か見える」までが 100ms 以内（体感の独立指標）

## 4. State Schema (Phase 1+ で参照)

Surface materialization 専用の state file は v0 では不要。CLI 側の
`~/.ato/apps/<package_id>/sessions/<id>.json` をそのまま読む。

ただし Phase 2 で WebView pool が乗ったとき、pool member の identity を追跡
するため Desktop プロセス内 in-memory state は必要:

```rust
// In-process only — persistence は v1+
struct SurfacePool {
    /// Pool 内の pre-warmed WebView. `available` の WebView は
    /// `about:blank` か skeleton をロード済み。
    available: Vec<PooledSurface>,
    /// 現在 pane に bind されている WebView. partition_id がキー。
    in_use: HashMap<String, ActiveSurface>,
}

struct PooledSurface {
    webview: wry::WebView,
    web_context: wry::WebContext,
    /// このプール member のスキーム。capsule の partition_id を取得した
    /// 後で hot-rebind する。
    placeholder_scheme: String,
    created_at: Instant,
}

struct ActiveSurface {
    webview: wry::WebView,
    partition_id: String,
    session_id: String,
    local_url: String,
    activated_at: Instant,
}
```

`SurfacePool` は `WebViewManager` の field として持ち、永続化はしない。
Desktop プロセス再起動で pool は空になる（cold path）。

## 5. Phase Timing 表現

PR 1 で建てた `PHASE-TIMING` モデルと同形式で、Desktop 側に
`SURFACE-TIMING` を追加する。

```
SURFACE-TIMING stage=subprocess_spawn elapsed_ms=148
SURFACE-TIMING stage=envelope_parse elapsed_ms=12
SURFACE-TIMING stage=pane_state_build elapsed_ms=34
SURFACE-TIMING stage=webview_create elapsed_ms=287
SURFACE-TIMING stage=webview_load_url elapsed_ms=23
SURFACE-TIMING stage=first_paint elapsed_ms=856
SURFACE-TIMING total elapsed_ms=1460
```

`ATO_SURFACE_TIMING=1` env で有効化、stderr のみ。GPUI / Wry のコールバックは
async / cross-thread が多いので、`PhaseStageTimer` 相当の RAII 型を
Desktop crate にも作る（`ato-cli` 側のものは crate boundary を超えないので
複製は許容、API 形は揃える）。

result_kind 値集合（CLI と整合）:

| 値 | 意味 |
|---|---|
| `materialized-surface` | pool hit + URL 切替のみ |
| `executed` | pool miss / 新規 WebView 生成 |
| `not-applicable` | 非 WebView surface（terminal, native 等） |

`prior_kind` extras（pool miss の理由）:

| 値 | 意味 |
|---|---|
| `pool-empty` | pool に WebView が無かった |
| `partition-mismatch` | 取得しようとした scheme と pool member が非互換 |
| `pool-disabled` | env で pool を無効化中 |

## 6. CLI / API 互換性

| 既存挙動 | v0 後 |
|---|---|
| `ato app session start` の subprocess | Phase 1 で **追加の fast path** が入るが subprocess も維持。fallback 経路として残る |
| `WebViewManager::sync_from_state` | Phase 2 で pool acquire を追加。pool miss 時は既存経路 |
| `BridgeProxy` の capability allowlist | 不変。pool acquire 時に bridge config を再 attach するだけ |
| `host_bridge.js` / adapter shims | 不変。pool member は generic profile で起動し、acquire 時に追加 inject |
| `ATO_DESKTOP_SESSION_ROOT` env override | 不変。Phase 1 の direct read もこの env を尊重 |

## 7. 設定 / declaration (v1+)

v0 では capsule.toml に新しい declaration を **追加しない**。Phase 0–3 は
すべて Desktop 内部の最適化。

v1+ で議論する候補:

```toml
[surface]
prefetch = true                # Phase 3 prefetch をこの capsule で有効にするか
warm_pool_member = true         # Phase 2 pool に常に枠を確保するか (pinned)
loading_shell = "minimal"       # "minimal" | "splash:<asset>" | "none"
```

これらは別 RFC（`SURFACE_DECLARATION` 等）に分離。本 RFC は**実機側のみ**を
扱う。

## 8. やらないこと（v0 スコープ外）

| 項目 | 先送り理由 |
|---|---|
| WebView pool の persistent 化（Desktop 再起動間で維持） | OS / WebView API の制約。daemon 化と一緒に v1+ |
| `ato run --reuse` の Desktop 経路への流入 | CLI APP_SESSION RFC v1 待ち |
| `--no-prefetch` / `--cold` 等の Desktop CLI flag | UX 議論が必要。v1 |
| service worker による offline / asset cache | 別 RFC |
| JS bundle splitting hint を capsule.toml で declare | フレームワーク固有色が強い。capsule-side の責務 |
| Wry → 別 WebView 実装（WKWebView 直 / Tauri stable channel 等）への移行 | 別 RFC、実測で必要性が出てから |
| Multi-window / multi-monitor surface materialization | v1+ |
| Capsule の `ato-cli` 統合バイナリ化（`ato-desktop` に CLI を embed） | 別 RFC（daemon / embed の検討） |

## 9. セキュリティ / 整合性

### 9.1 Pool member の partition / scheme 切替

Pool 内 WebView は generic な `capsule_pool_<n>` scheme で起動する。capsule
への bind 時に partition_id を実 capsule のものに切替えるが、これは Wry の
public API で許可されていない可能性がある。許可されていなければ Phase 2 は
**新 WebView 生成 + pool に「準備済みの web context」を持っておく**形に
スコープを縮める（context 再利用だけで生成コストの一部削減）。

### 9.2 Prefetch の attack surface

Phase 3 prefetch は host process（Desktop）から `local_url` への HTTP GET を
発火する。これは:

- ターゲットは常に `127.0.0.1:<port>` または session record の `local_url`
  のみ
- 他のホストへの fetch は許可しない（loopback 限定）
- prefetch のレスポンス body は破棄（warming 目的なので保持しない）
- credential / cookie は inject しない

これらを Phase 3 PR の中で明示的に enforce する。

### 9.3 Direct disk read の race

Phase 1 で Desktop が session record を直読する場合、**CLI が record を
書いている最中**を読む可能性がある。CLI 側は既に atomic temp+rename で
書いているので、partial read は起きない。ただし Desktop 側でも:

- read 時に `serde_json::from_slice` 失敗 → fallback to subprocess
- record の `schema_version` が unset / `< 2` → fallback to subprocess

を必ず入れる。validate fail も同様に subprocess fallback。

### 9.4 Pool exhaustion / DoS

ユーザが大量に pane を開いて pool を吐かせると、新規生成 + 巨大 GPU メモリ
消費 / WebView crash の risk。v0 では:

- pool size を固定上限（v0 では `available + in_use <= 8` 程度）
- 上限超過時は **既存経路にフォールバック**（pool を bypass）
- 古い `in_use` を強制終了する logic は持たない（ユーザの作業を勝手に
  消さない）

## 10. 受け入れ条件 (Acceptance Criteria)

### 10.1 Phase 0 (Measurement)

PR 1 と同じ性質。実装変更ではなく実測値の確定が成果物。

- [ ] byok-ai-chat warm Desktop click → first-paint で `SURFACE-TIMING`
  が出る
- [ ] cold / warm-reuse 各 5 回計測し、stage 別の中央値を README または
  RFC §1.1 に追記
- [ ] §1.1 の推定値との差が 50ms 以内 (実測 vs 推定の妥当性確認)

### 10.2 Phase 1 (Subprocess elimination)

- [ ] reuse-eligible session が disk にあるとき、`subprocess_spawn` stage
  が出ない（直読 fast path が効いている）
- [ ] reuse-ineligible のとき、subprocess fallback が走り cold path と
  同じ結果を返す
- [ ] byok-ai-chat warm reuse の click → first-paint が 100–150ms 短縮
  （Phase 0 baseline 比）
- [ ] session record 破損 / 5 条件 fail のいずれでも crash しない

### 10.3 Phase 2 (WebView Pool)

- [ ] pool size = 1 の状態で同じ capsule を 2 回開くと、2 回目は
  `result_kind=materialized-surface`
- [ ] pool size = 1 で 2 つ別 capsule を同時に開くと、片方は pool hit、
  もう片方は新規生成（fallback）。両方とも正常動作
- [ ] partition_id / bridge / preload の整合性が破壊されていない（既存
  bridge unit test 全 pass）
- [ ] Desktop 起動直後（pool 空）の 1 回目 click は cold path と同等の
  時間

### 10.4 Phase 3 (Prefetch + App Shell)

- [ ] `local_url` が判明した時点から prefetch が始まり、`SURFACE-TIMING`
  に `prefetch_started` / `prefetch_completed` が出る
- [ ] skeleton 表示が click 後 100ms 以内
- [ ] byok-ai-chat warm の `webview_load_url → first_paint` が 200ms 以上
  短縮（Phase 0 baseline 比）

### 10.5 全 Phase 後の総合

- [ ] byok-ai-chat warm click → first interactive paint が **< 1s** に
  収まる（baseline 3.5s 比で 2.5s+ 削減）
- [ ] cold（Desktop 起動直後の初回 click）は現状 ±100ms 以内（regression
  なし）

## 11. 移行パス

各 Phase は独立に shippable。

- Phase 0 (PR 3): 計測基盤のみ。挙動変更なし。低リスク
- Phase 1 (PR 4): direct read fast path。subprocess fallback があるので
  互換性安全
- Phase 2 (PR 5): pool。pool miss fallback があるので互換性安全。ただし
  Wry / WKWebView の API 制約で実装難度が高い可能性
- Phase 3 (PR 6): prefetch + skeleton。両方とも best-effort で fallback
  あり

各 Phase の前に:
- 前 Phase の acceptance を満たしているか確認
- 数値で次の Phase の必要性を判断（Phase 1 の効果が小さければ Phase 2 を
  優先する等の動的優先順位付けを許容）

## 12. オープンクエスチョン

- **Phase 1 の 5 条件 validation を Desktop で再実装するか CLI に subprocess
  で問い合わせるか**: 完全再実装は CLI と Desktop で 2 重管理になる。CLI に
  subprocess する場合 Phase 1 のメリットが薄れる。中間案として `ato session
  validate <handle> --json` のような fast subcommand を切り出して CLI 起動
  オーバーヘッドを最小化する案もある
- **Phase 2 の partition_id hot-rebind が Wry で許可されない場合の代替**:
  pool 内 WebView を generic profile で起動し、capsule への bind 時に
  全 preload を inject し直す形で済ませるか、それとも pool 自体を諦めて
  WebContext のみ pool するか
- **Phase 3 の prefetch を CLI 側に移すべきか**: `ato app session start` の
  内部で prefetch すれば Desktop 改修不要。ただし prefetch のキャンセル
  semantics（user が click を取り消した場合）が CLI 側では取れない
- **GPUI 側の paint pipeline と WebView の合成タイミング**: GPUI の
  `Window::request_animation_frame` が WebView ready signal とどう同期する
  か未調査。`first_paint` の正確な計測は GPUI 側の hook が要る可能性

## 13. 関連仕様 / 実装参照

- [BUILD_MATERIALIZATION.md](BUILD_MATERIALIZATION.md) — prior art。3 層
  モデル / `[build]` 宣言 / `materializations.json` のパターンを本 RFC でも
  踏襲
- [APP_SESSION_MATERIALIZATION.md](APP_SESSION_MATERIALIZATION.md) — Surface
  の上流。session record / launch_digest / 5 条件 validation の output を
  本 RFC が消費する
- `apps/ato/crates/ato-desktop/src/orchestrator.rs:370` —
  `resolve_and_start_capsule` の hot path。Phase 1 の挿入位置
- `apps/ato/crates/ato-desktop/src/orchestrator.rs:603` — `run_ato_json`
  の subprocess invocation。Phase 1 で fast path が回避する対象
- `apps/ato/crates/ato-desktop/src/webview.rs:376` —
  `WebViewManager::sync_from_state`。Phase 2 の pool 挿入位置
- `apps/ato/crates/ato-desktop/src/webview.rs:363` (`WebViewBuilder`) —
  pool member 生成の参照点
- `apps/ato/crates/ato-desktop/CLAUDE.md` — Desktop 内部のアーキテクチャ
  ガイド。Phase 2 の WebView lifecycle 設計時に参照
- `apps/ato/crates/ato-cli/src/application/launch_materialization.rs` —
  CLI 側の 5 条件 validation。Phase 1 で再利用するか subprocess 化するか
  の議論対象
