# 📄 Desktop Surface Materialization Specification

**Document ID:** `SURFACE_MATERIALIZATION`
**Status:** Draft v0.2
**Target:** ato-desktop v0.5.x
**Last Updated:** 2026-04-29

> **Scope.** This RFC covers only the latency between the user clicking a
> capsule launcher in `ato-desktop` and the capsule's UI becoming visible
> and interactive. It does **not** redefine the WebView security model,
> the `capsule<partitionId>://` protocol, or the bridge / IPC contracts —
> those are taken as-is from the existing implementation.

> **v0.2 changes from v0.1**:
> - Phase 2 reframed: **session-keyed Surface Retention is the headline,
>   blank WebView pool is a fallback spike** (§2.1, §3.3).
> - `partition_id` hot-rebind is explicitly **prohibited** for v0
>   cross-capsule reuse — `about:blank` does not clear cookies / scheme
>   handlers / data stores reliably (§9.1).
> - Phase 0 acceptance no longer gates on "estimate vs. measurement <
>   50 ms"; replacing the estimate **is** the deliverable (§3.1).
> - First-paint / navigation-finished / first-interactive metrics are
>   separated; first-interactive stays best-effort (§5).
> - Phase 1 prefers a shared validation helper crate over re-implementing
>   `LaunchSpec` canonicalization in Desktop (§3.2).
> - Skeleton is a **GPUI native overlay** first; WebView-document shell
>   is deferred to v1 (§3.4).
> - Prefetch acceptance softened to "best-effort, drop if no measurable
>   gain" (§3.4, §10.4).
> - §1.1 baseline numbers re-marked as **pre-measurement hypothesis**.

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
**~3.5 秒**残っている。

#### Pre-measurement hypothesis (NOT measured — to be replaced by Phase 0)

PR 1 が build phase で `prepare_session_execution` を 0ms と実測したように、
以下は **推定** であり **設計の前提ではない**。Phase 0 (PR 3) で実測値で
置き換えるためのスタート地点に過ぎない。

```
Desktop click
  ├─ ato CLI subprocess spawn + envelope return:    ~10 ms (warm reuse)
  ├─ ato CLI 自体の cold-start (binary load, clap):  ~150 ms ★ (1) [hypothesis]
  ├─ orchestrator が envelope を parse / pane state 構築:  ~50 ms [hypothesis]
  ├─ WebView 生成 (Wry, WKWebView 初期化):              ~300 ms ★ (2) [hypothesis]
  ├─ WebView が `http://127.0.0.1:<port>/` を fetch:    ~50 ms [hypothesis]
  ├─ Next.js SSR の first / response:                    ~300 ms ★ (3) [hypothesis]
  ├─ JS bundle download + parse + execute + hydrate:   ~1200 ms ★ (4) [hypothesis]
  └─ first interactive paint:                            ~300 ms [hypothesis]
                                                        ────────
observed total                                         ~3500 ms
accounted hypothesis                                   ~2360 ms
unattributed                                           ~1140 ms
```

★ がついた 4 箇所を、**仮に推定が当たっていれば**支配項として扱える。
ただし PR 1 の `prepare_session_execution` 実測（300–500ms 推定 → 実測 0ms）
の前例から、**Phase 0 で実測するまでこれらの数値で設計判断をしない**。

設計上の含意:

- 推定の中で最大は `JS bundle exec / hydrate ~1200ms`。これは Ato 直接
  制御不能で、Surface Retention で **そもそも実行を回避する** のが本質
  （§2.1）。
- WebView 生成 ~300ms は materialization で消せる。ただし retention
  （session-keyed）の方が pool（generic）より効果が大きい（§2.1）。
- subprocess + ato cold-start ~150ms は Phase 1 で消す候補。だが Phase 0
  の実測で 50ms 未満なら優先度を下げる。

| 問題 | 具体 |
|---|---|
| 同一 capsule への click ごとに WebView を再生成 | `WebViewManager::sync_from_state`（webview.rs:376）で pane の partition_id が変わるたびに WebView を tear down + 新規生成。pool / preload なし |
| ato CLI subprocess が常に必要 | `orchestrator::resolve_and_start_capsule`（orchestrator.rs:370）が `ato app session start` を spawn。warm reuse でも fork+exec の固定コスト ~150ms |
| 初回 fetch まで surface が空 | WebView は about:blank → URL 切替 → SSR 待ち の直列。skeleton / app shell が無く体感が長い |
| URL prefetch 未実装 | session record から `local_url` が取れた瞬間に並行 fetch を開始すれば SSR を warm にできるが、現在は WebView 生成後に初めて HTTP が走る |

### 1.2 設計方針

- **Surface は phase ではなく artifact**: WebView は毎回作る UI 部品ではなく、
  capsule session に対応する materialized resource として扱う。
- **Retention > Pool**: 効くのは「app document が既に hydrate 済みの WebView
  をそのまま再 attach する」retention であり、blank WebView の generic pool は
  WebView 生成コスト（~300ms 推定）しか消せない。本 RFC は **session-keyed
  Surface Retention を本命**、generic pool を fallback / spike とする
  （§2.1, §3.3）。
- **Same-partition only for v0**: WebView の `partition_id` は isolation
  境界。`about:blank` に戻しても cookies / localStorage / scheme handler /
  data store が完全に清掃される保証はない。**v0 では cross-capsule reuse を
  禁止し、retention は same session_id / same partition_id 内のみで成立**
  させる（§9.1）。
- **3 層モデルへの整合**: 宣言（`capsule.toml [surface]` v1+）／解決結果
  （session record）／実機状態（in-process retention table + (v0.next)
  pool）。
- **Measurement first**: PR 1 で build phase の sub-stage timing を入れたのと
  同じく、Phase 0 として `SURFACE-TIMING` を Desktop 側に入れて 3.5s の
  decomposition を **実測で確定** してから次の Phase の優先順位を決める
  （§3.1）。Phase 0 の deliverable は推定の置換であり、推定との一致では
  ない。
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

### 2.1 Retained Surface (本 RFC v0 の本命)

Retained Surface は、pane が閉じても **対応する `(session_id, partition_id)`
の WebView を hidden state で in-process に保持** し、再 click 時に attach /
reveal だけで表示する仕組み。

```
session_id + partition_id + local_url
  → already navigated, hydrated WebView
  → hidden retention table
  → pane click 時に attach + reveal
```

これは generic な blank WebView pool との **本質的な違い**:

| 軸 | Blank Pool | Retained Surface |
|---|---|---|
| WebView 生成 | warm | warm |
| Wry / WKWebView 初期化 | 回避 | 回避 |
| navigation (`load_url`) | 必要 | **回避** |
| Next.js SSR | 必要 | **回避** |
| JS bundle parse / exec | 必要 | **回避** |
| hydration | 必要 | **回避** |
| 期待効果 | ~300 ms (推定) | **navigation + JS exec + hydrate を全部消す** |

retention の対象は「同じ session_id / partition_id への再表示」のみ
（§9.1）。cross-capsule / cross-partition の reuse は v0 では禁止。

### 2.2 Surface Identity と Reuse 条件

retention table のキーは:

```
SurfaceKey = (session_id, partition_id)
```

retain したまま再 click された時の reuse 条件 (5):

1. retention entry が存在し expire していない（v0 default: 5 分 TTL）
2. WebView が alive（Wry handle が valid、render プロセス未 crash）
3. App Session 側の session_id が依然 reusable
   （APP_SESSION_MATERIALIZATION §9.1 の 5 条件 pass — 直読 fast path で確認）
4. URL が変わっていない（session record の `local_url` が retention 時と同一）
5. partition_id が変わっていない（pane が同一 capsule scheme を要求）

1 つでも fail → retention drop して新規 WebView を生成（既存経路）。

### 2.3 Native Loading Overlay

skeleton は **GPUI の native overlay** として実装する。WebView 内 document
として skeleton を出すと document replacement で flash / blank が走るため
（§3.4）。

```
pane click
  ├─ GPUI native overlay を pane bounds 上に即表示（< 16 ms）
  ├─ 背後で WebView を attach / create
  ├─ navigation_finished + first_paint_signal を待つ
  └─ overlay を fade out
```

retention hit 時は overlay は不要 or 数 frame だけ表示して即 hide。
retention miss 時は SSR + JS exec の間 overlay が見える。

### 2.4 Generic WebView Pool — fallback / spike (Phase 2B)

Pool は retention とは独立の補助層。retention が miss した時の WebView
**生成コスト** だけを消す。app document の hydration コストは消えない。

Wry / WKWebView の制約により partition_id の hot-rebind が許されない場合、
pool は実用にならない可能性がある。本 RFC は pool を **Phase 2B の spike**
として位置づけ、Phase 0 の実測 + Phase 2A retention の効果次第で実装する
かを判断する（§3.3）。

### 2.5 Surface Prefetch

session record から `local_url` が取れた瞬間（subprocess の envelope 受領
時、または disk 直読時）、Desktop の async runtime で **HTTP GET を 1 回
先行して打つ**。Next.js server の SSR cache を warm にする狙い。

prefetch は **best-effort**:
- 失敗しても表示は壊れない
- loopback (`127.0.0.1` / `localhost`) のみ
- credential / cookie は inject しない、レスポンス body は破棄
- Phase 0 / Phase 3 の実測で効果が無ければ Phase 3 から外す（§3.4）

## 3. 実装フェーズ

このRFCは 4 つの phase に分けて段階的にロールアウトする。各 phase は
独立に shippable で、計測値で次の phase の必要性を判断する。

### Phase 0: Measurement (PR 3)

**目的**: 3.5s の decomposition を **実測値で確定** する。§1.1 の hypothesis を
置き換える。これが本 phase の deliverable であり、実測と推定の一致は目的
ではない（PR 1 build phase で `prepare_session_execution` 推定 300–500ms
が実測 0ms だった前例 — 推定との差を発見することこそが価値）。

**変更**:

- Desktop 側に `SURFACE-TIMING` を追加（CLI の `PHASE-TIMING` と同形式、
  stderr 出力、`ATO_SURFACE_TIMING=1` で有効）
- optional: `ATO_SURFACE_TIMING_FILE=<path>` で ndjson 出力（GUI run の
  stderr が見えにくい環境を想定。実装が重ければ stderr のみで開始）
- session root 解決は CLI と同じ env (`ATO_DESKTOP_SESSION_ROOT`) を尊重
  （app_control/session.rs:951 と一致）

**計測点** — 用途別に分類:

| Stage | 必須 / Optional | 取り方 |
|---|---|---|
| `click_start` | 必須 | GPUI の click handler 入口 |
| `session_resolved` | 必須 | session envelope 取得完了（subprocess return or direct read return） |
| `subprocess_spawn` | 必須 (subprocess 経路時) | `run_ato_json` 開始 → exec 戻り |
| `envelope_parse` | 必須 | `serde_json::from_str` 完了 |
| `pane_state_build` | 必須 | `build_launch_session` 完了 |
| `webview_create_start` / `_end` | 必須 | Wry `WebViewBuilder::build` の前後 |
| `navigation_start` | 必須 | `webview.load_url(local_url)` 呼び出し |
| `navigation_finished` | 必須 | Wry の `on_navigation_complete` callback |
| `first_dom_content_loaded` | 可能なら | injected script の `DOMContentLoaded` |
| `first_paint_signal` | best-effort | injected `PerformanceObserver('paint')` → host_bridge 経由 postMessage |
| `first_interactive_signal` | optional | app-specific marker (Next.js hydrate 完了 hook 等)。capsule 側協力なしには取れないことを許容 |
| `total` | 必須 | click_start からの累計 |

**byok-ai-chat 実測の進め方**:
- cold (Desktop 起動直後の初回 click) を 5 回
- warm-reuse (session record 既存 + retention なし) を 5 回
- median と p90 を記録
- top 2 bottlenecks を特定

**Out of scope**: implementation の最適化はしない。計測のみ。

**Acceptance**:

- [ ] `ATO_SURFACE_TIMING=1` で必須 stage 行が出る
- [ ] cold / warm 各 5 回の median / p90 を取得し RFC §1.1 の hypothesis 表を
  実測値で書き換える
- [ ] top 2 bottlenecks を §1.1 末尾に記載
- [ ] その実測に基づき Phase 1–3 の優先順位を **再決定** する（推定どおりで
  あることは要求しない）

**Phase 0 の出力**は次の Phase の優先順位を決める判定材料そのもの。

### Phase 1: Subprocess elimination (PR 4)

**目的**: ato CLI の subprocess + cold-start を warm reuse path で消す。

**前提**: Phase 0 の実測で `subprocess_spawn` + `envelope_parse` の合計が
有意に大きいことを確認してから着手（推定 ~150ms）。50ms 未満であれば
Phase 2A retention を優先し、Phase 1 を後送りする。

**実装方針 — 優先順位**:

#### 第一候補: Shared validation helper crate

CLI 側 `application/launch_materialization.rs::prepare_reuse_decision` を
**新 crate `ato-session-core`** （または既存 `capsule-wire` への追加）に
切り出し、CLI と Desktop の両方から呼ぶ。これが筋として一番きれい:

- validation logic が CLI / Desktop で一致する保証
- `LaunchSpec` canonicalization が一箇所に閉じる
- digest 計算が drift しない
- 将来 CLI v1 で reuse path が拡張されても Desktop に自動反映

cost: capsule-wire か新 crate のスコープ拡大、依存整理。1–2 日。

#### 第二候補: Record-only validation in Desktop

shared crate 化が困難な場合、Desktop は **record の存在 + 浅い検証** のみ:

- `schema_version >= 2`
- `handle / target` match
- `launch_digest` フィールドが record に **存在する**
- `pid alive`
- `process_start_time` match
- healthcheck success

ただし **current desired `launch_digest` は再計算しない**。「現在の manifest /
source が record の launch_digest と一致するか」の確認はしない（できない）。
これは:

- ✅ 同じ手元の capsule を再表示するときの fast path として機能する
- ❌ manifest / source 変更検知が無いので、次の subprocess が走ったタイミングで
  CLI 側の 5 条件 validation に頼る
- ⚠️ Phase 1 の効果は「再表示の subprocess を回避するだけ」に限定される

RFC v0.2 では this を hard limitation として明示する。manifest / source
change が予想されるシナリオ（`ato run` から開発中など）では subprocess
fallback に倒すための short TTL（例: 30 秒）を retention entry に持たせる。

#### 避ける案: Desktop 独自 LaunchSpec canonicalization

drift しやすく、CLI と Desktop の bug fix が必ず双方で必要になる。**v0 では
やらない**（明示的に Out of scope）。

**変更点**:

- `orchestrator.rs:370` `resolve_and_start_capsule` の冒頭に fast-path
  branch を追加（または別関数 `try_resolve_from_record`）
- record 直読 + validation。fail なら **必ず** subprocess fallback
- subprocess fallback 経路は現状を変更しない
- `SURFACE-TIMING` で `subprocess_spawn` が出ない / 出る を区別できる

**Out of scope**:

- 失敗時 fallback の挙動変更
- `--respawn` 等の flag（CLI v1 待ち）
- daemon 化（別 RFC）
- manifest / source change の自前検知

**Acceptance**:

- [ ] Phase 0 baseline と比較して `subprocess_spawn` + `envelope_parse` 合計が
  reuse-eligible 経路で 0 に近い
- [ ] 第一候補 / 第二候補のどちらを採ったか RFC §13 / TODO に明記
- [ ] record 破損 / 5 条件 fail / TTL 切れ で必ず subprocess fallback
- [ ] Phase 0 の fallback path 数値が劣化していない（regression なし）

### Phase 2A: Session-Keyed Surface Retention (PR 5, 本命)

**目的**: 同じ `(session_id, partition_id)` への再 click で WebView 生成 +
navigation + JS bundle exec + hydrate を **全部消す**。本 RFC v0 の最大の
optimization。

**変更**:

- `WebViewManager` に `RetentionTable: HashMap<SurfaceKey, RetainedSurface>`
  を追加
- pane close (= `WebView` を destroy しようとする箇所) で:
  - retention 条件を満たす場合 → destroy せずに hidden 化して retention
    table に移す
  - 満たさない場合 → 既存経路で destroy
- pane click (= 新規 `WebView` を作ろうとする箇所) で:
  - retention table に hit があり §2.2 の 5 条件すべて pass → attach +
    reveal だけで完了
  - miss → 既存経路で新規生成

**Retention の TTL / LRU**:

- v0 default TTL: **5 分**（idle 時間。最後に attach 解除された時点から）
- 上限: 同時 retain 数 **8**（OS の WebView 上限・GPU メモリを考慮した
  保守値）。超過時は LRU で oldest を destroy
- 上限超過時に新規 retain を試みた場合、retention をスキップして単純
  destroy（regression にはならない）

**設計上の難所** (Phase 2A 固有):

- **Hidden state での GPU / network コスト**: WebView を `set_visible(false)`
  しても render プロセスは生きている。Next.js の SSR 接続が切れずに network
  使用が続く可能性がある → 最初は許容、Phase 0 後の実測で問題なら TTL を
  短くする
- **session 側 ready check との同期**: APP_SESSION_MATERIALIZATION の 5
  条件（pid alive 含む）が retention check 側でも要る。同じ shared helper
  を使う（§3.2 第一候補）
- **pane close vs. capsule shutdown の区別**: ユーザが明示的に
  `ato app session stop` した場合は retention も同時破棄する（既存の stop
  flow に hook）

**Out of scope (v0)**:

- cross-partition reuse（§9.1 で明示禁止）
- multi-window / multi-monitor で同じ retention を共有
- 永続化（Desktop 再起動を超えて保持）

**Acceptance**:

- [ ] 同じ capsule/session を pane close → 5 分以内に再 open すると
  `result_kind=materialized-surface`
- [ ] `webview_create` stage が出ない（または < 10ms）
- [ ] `navigation_finished` stage が出ない（または既存 document を再表示
  するだけで < 10ms）
- [ ] click → first_visible_signal が **< 100ms**
- [ ] retention TTL 切れ後の再 open は新規生成パスに倒れる（cold path と
  同じ数値）
- [ ] cross-partition での誤 reuse が発生しない（§9.1）
- [ ] `ato app session stop` 後の再 open は新規生成（retention drop されている）

### Phase 2B: WebView / WebContext Pool — spike (PR 5b)

**目的**: Phase 2A retention が miss した場合の WebView **生成コスト** だけを
追加で削る試み。app document の hydration コストには効かない（retention の
代替にはならない）。

**前提**: Phase 0 の実測 + Phase 2A の効果で、retention miss path の
`webview_create` がまだ問題サイズなら着手。先に Phase 2A の数値を見る。

**実装方針 — spike として進める**:

1. Wry / WKWebView の API で **partition / scheme / data store の hot-rebind
   が安全に可能か** を検証する小さな PR（spike）
2. 安全と確認できたら blank WebView pool（generic profile、acquire 時に
   bind）を実装
3. 安全でなければ pool は **`WebContext` のみ**（`WebView` 本体は使い回さ
   ない）に縮小し、acquire 時に新規 `WebView` を作るが共有 `WebContext` を
   渡す → 期待効果は数十ms に下がる

**v0 の安全ルール** (§9.1 と整合):

| Reuse type | v0 |
|---|---|
| same session_id / same partition_id | OK (Phase 2A retention) |
| same capsule / same partition / 別 session | spike 後に判断 |
| different capsule / different partition | **NG** (禁止) |
| generic blank WebView → capsule partition rebind | spike 後まで NG |

**Out of scope**:

- pool size の dynamic 調整: v1
- multi-window / multi-monitor 配慮: v1+

**Acceptance** (spike 結果次第で skip 可):

- [ ] Wry / WKWebView の rebind 制約を文書化（RFC §13 に追記）
- [ ] rebind 不可と判明した場合: `WebContext` のみ pool 化、または Phase 2B
  自体を v1 に降格
- [ ] rebind 可なら pool hit で `webview_create` < 10ms
- [ ] cross-capsule で誤 reuse が発生しない unit test

### Phase 3: Native Loading Overlay + Prefetch (PR 6)

**目的**: retention miss path の **体感** を改善する。retention hit path には
原則 overlay は不要。

#### 3a. Native Loading Overlay (本命)

**変更**:

- pane click 直後に **GPUI の native overlay** を pane bounds 上に即表示
  （< 16ms = 1 frame）
- 背後で WebView を attach / create / navigate
- `navigation_finished` + `first_paint_signal` で overlay を fade out

**WebView document skeleton にしない理由**:

- WKWebView では document replacement で flash / blank / navigation delay
  が出やすい
- skeleton document は capsule の document context を汚す（cookies / scheme
  handler）
- CSP 制約 / cross-origin policy の影響を受ける
- `data:` URI / `capsule://__loading__/` どちらでも **first-contentful-paint
  そのものは早くならない** ことが多い

native overlay なら:

- WebView navigation と完全独立
- capsule document を一切汚さない
- 即時表示（GPUI の next-frame 描画）

**Out of scope**:

- アニメーション・design tokens の凝った skeleton（v1）
- WebView 内 skeleton（v1+）
- overlay と WebView の cross-fade synchronisation（GPUI と Wry の paint
  pipeline 同期。v1）

**Acceptance**:

- [ ] click → overlay visible が < 100ms
- [ ] retention hit 時は overlay が出ない（または 1 frame だけで disappear）
- [ ] overlay の dismiss が `navigation_finished` または `first_paint_signal`
  に同期している（v0 では navigation_finished のみで OK）

#### 3b. Surface Prefetch (best-effort)

**変更**:

- session envelope の `local_url` が判明した直後に `tokio::spawn` で
  `http::Client::get(local_url)` を fire-and-forget
- レスポンス body は **破棄**（warming 目的のみ）
- credential / cookie は **inject しない**
- 対象は **loopback (`127.0.0.1` / `localhost`) のみ**。それ以外の host へは
  fetch しない

**期待効果と制約**:

- Next.js の SSR cache が warm になれば、後続の WebView fetch が早くなる
- ただし以下のリスクで効果は不確実:
  - host process の GET と WebView navigation で cookie / header / cache
    semantics が違う場合がある
  - GET が side-effect を持つ capsule では prefetch が誤作動を引き起こしうる
    （v0 では loopback / `local_url` のみに限定して mitigation）
  - JS bundle の parse / hydrate コストは prefetch では消えない（document
    だけが warm）

**v0 の取り扱い**:

prefetch は **best-effort**。Phase 0 / Phase 3 完了後に実測で効果が示せない
場合は **Phase 3 から外す**（実装は残しても feature flag で off）。

**Out of scope**:

- WebView 内での bundle prefetch（service worker 経由）
- offline-first 対応
- HTTP/2 push / Early Hints 等の transport 最適化

**Acceptance** (low-bar、実測で効果を判断):

- [ ] `ATO_SURFACE_TIMING=1` で `prefetch_started` / `prefetch_completed`
  stage が出る
- [ ] prefetch が失敗しても表示は壊れない (subprocess fallback と同列の
  "fail-soft" 扱い)
- [ ] loopback 以外の host へ送信していないことを unit test で確認
- [ ] credential / cookie が inject されていないことを unit test で確認
- [ ] 実測で効果がなければ Phase 3 から外す決定を §13 に記録する

## 4. State Schema (Phase 1+ で参照)

Surface materialization 専用の state file は v0 では不要。CLI 側の
`~/.ato/apps/<package_id>/sessions/<id>.json` をそのまま読む。

Phase 2A retention と Phase 2B pool 用の Desktop プロセス内 in-memory
state:

```rust
// In-process only — persistence は v1+ (Desktop 再起動で空に戻る)

/// Phase 2A: session-keyed retained surfaces. Same session_id /
/// partition_id への再 click でこのテーブルから attach する。
struct RetentionTable {
    entries: HashMap<SurfaceKey, RetainedSurface>,
}

#[derive(Hash, Eq, PartialEq)]
struct SurfaceKey {
    session_id: String,
    partition_id: String,
}

struct RetainedSurface {
    webview: wry::WebView,         // hidden, not destroyed
    web_context: wry::WebContext,
    local_url: String,             // retention 時の URL（変わったら drop）
    retained_at: Instant,          // TTL 計算の起点
    partition_id: String,
}

/// Phase 2B (spike): generic pool. retention miss 時の WebView 生成
/// コストだけを削る補助。
struct SurfacePool {
    available: Vec<PooledSurface>,
}

struct PooledSurface {
    webview: wry::WebView,
    web_context: wry::WebContext,
    created_at: Instant,
}
```

`RetentionTable` と `SurfacePool` は `WebViewManager` の field として持ち、
永続化はしない。

retention の eviction policy (v0):

- TTL: 5 分（最後の attach 解除から）
- LRU: 同時 retain 数上限 8
- Explicit drop: `ato app session stop` 経由で session が止まった時
- Force drop: `ato-desktop` 終了時 (Drop impl)

## 5. Phase Timing 表現

PR 1 で建てた `PHASE-TIMING` モデルと同形式で、Desktop 側に
`SURFACE-TIMING` を追加する。`ATO_SURFACE_TIMING=1` env で有効化、stderr
が基本 (optional に `ATO_SURFACE_TIMING_FILE=<path>` で ndjson 出力)。

GPUI / Wry のコールバックは async / cross-thread が多いので、
`PhaseStageTimer` 相当の RAII 型を Desktop crate にも作る（`ato-cli` 側の
ものは crate boundary を超えないので複製は許容、API 形は揃える）。

### 5.1 Stage 名と取り方

「latency の物差し」と「reuse path の判定」を兼ねる。Phase 0 で記録、
Phase 1–3 で削った効果を比較できるよう値集合を固定する:

| Stage | Phase 0 必須 | 取り方 |
|---|---|---|
| `click_start` | ✅ | GPUI click handler 入口 |
| `session_resolved` | ✅ | session envelope 取得完了 |
| `subprocess_spawn` | ✅ (subprocess 経路時) | `run_ato_json` 開始 → exec 戻り |
| `envelope_parse` | ✅ | `serde_json::from_str` 完了 |
| `pane_state_build` | ✅ | `build_launch_session` 完了 |
| `retention_lookup` | Phase 2A 後必須 | retention table 検索 |
| `retention_attach` | Phase 2A 後 (hit 時) | hidden → visible 化 |
| `webview_create_start` / `_end` | ✅ | `WebViewBuilder::build` 前後 |
| `navigation_start` | ✅ | `webview.load_url(local_url)` |
| `navigation_finished` | ✅ | Wry の `on_navigation_complete` |
| `first_dom_content_loaded` | 可能なら | injected `DOMContentLoaded` |
| `first_paint_signal` | best-effort | `PerformanceObserver('paint')` → bridge |
| `first_interactive_signal` | optional | app 側 marker（capsule 協力に依存） |
| `prefetch_started` / `_completed` | Phase 3 後 | `tokio::spawn` 前後 |
| `total` | ✅ | `click_start` からの累計 |

### 5.2 result_kind 値集合 (CLI と整合)

| 値 | 意味 |
|---|---|
| `materialized-surface` | Phase 2A retention hit (本命) |
| `executed` | retention miss → 新規 WebView 生成 |
| `not-applicable` | 非 WebView surface（terminal, native 等） |

### 5.3 `prior_kind` extras (retention miss の理由)

| 値 | 意味 |
|---|---|
| `retention-empty` | retention table に entry が無かった |
| `retention-ttl-expired` | entry はあったが TTL 切れ |
| `retention-session-stale` | session 側の 5 条件 fail (APP_SESSION_MATERIALIZATION) |
| `retention-url-changed` | retain 時と異なる `local_url` を要求 |
| `retention-partition-changed` | retain 時と異なる `partition_id` を要求 |
| `retention-webview-dead` | retained WebView の render プロセスが crash |
| `retention-disabled` | env で retention を無効化中 |
| `pool-empty` | (Phase 2B 実装時) |
| `pool-disabled` | (Phase 2B 実装時) |

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

### 9.1 Same-partition only — partition_id hot-rebind の禁止

WebView の `partition_id` は **isolation 境界** であり、cookies / localStorage
/ scheme handler / data store がここで分離される。`about:blank` への
navigate では state が完全に清掃される保証はなく、特に WKWebView では
WKWebsiteDataStore が WebView 単位で fixed なため、cross-capsule reuse は
**安全でない**。

v0 の安全ルール:

| Reuse type | v0 |
|---|---|
| same session_id / same partition_id | ✅ OK (Phase 2A retention) |
| same capsule / same partition / 別 session | spike 後に判断 |
| different capsule / different partition | ❌ 禁止 |
| generic blank WebView → capsule partition への hot-rebind | ❌ Phase 2B spike 後まで禁止 |

retention table のキーは `(session_id, partition_id)` の組であり、cross-
key の reuse は実装上も発生しない（`HashMap` lookup 自体が不一致を返す）。

### 9.2 Retention の attack surface

Phase 2A retention で WebView が hidden state で生き続ける間:

- bridge の capability allowlist は retain 前と同一のまま継続
- credential / cookie は capsule の WKWebsiteDataStore に閉じる
- `ato app session stop` で session が止まったら retention も即 drop（§5
  state diagram）
- TTL 切れ entry は LRU で destroy（§4 schema）
- Desktop プロセス終了時に Drop impl で全 retention を destroy

retention 中に capsule 側のセキュリティ poll（auth expiry など）が必要な
場合は、capsule 自身の責務（既に WebView 内の JS で動く）。Desktop は
retention 期間中 host_bridge を停止しない。

### 9.3 Prefetch の attack surface

Phase 3 prefetch は host process（Desktop）から `local_url` への HTTP GET を
発火する:

- ターゲットは **`127.0.0.1` / `localhost` のみ**。それ以外は parse 段階で
  弾く（unit test で enforce）
- prefetch のレスポンス body は破棄（warming 目的のみ）
- credential / cookie は inject しない（Desktop は capsule の cookie jar を
  持っていないので元々不可能だが unit test で確認）
- prefetch を発火する trigger は session envelope 受領時のみ。ユーザ未操作
  の自動発火はしない（§3.4）

### 9.4 Direct disk read の race

Phase 1 で Desktop が session record を直読する場合、**CLI が record を
書いている最中**を読む可能性がある。CLI 側は既に atomic temp+rename で
書いているので、partial read は起きない。ただし Desktop 側でも:

- read 時に `serde_json::from_slice` 失敗 → subprocess fallback
- record の `schema_version` が unset / `< 2` → subprocess fallback
- 5 条件 validation のいずれかが fail → subprocess fallback
- TTL 切れ → subprocess fallback

を必ず入れる。

### 9.5 Retention exhaustion / DoS

ユーザが大量に pane を開いて retention を吐かせると、hidden WebView 累積で
GPU メモリ枯渇 / WKWebView crash の risk。v0 では:

- retention size を固定上限（v0 では同時 retain 数 8、§4）
- 上限超過時 LRU で oldest を destroy
- TTL 5 分（idle 後）で auto-evict
- 古い `in_use` を強制終了する logic は持たない（ユーザの作業を勝手に
  消さない）

### 9.6 Pool exhaustion (Phase 2B 実装時)

Phase 2B pool は spike 段階。実装する場合は §9.5 と同等の上限ルールを
適用し、上限超過時は新規生成 fallback。pool の hidden WebView も retention
の上限カウントに合算する。

## 10. 受け入れ条件 (Acceptance Criteria)

### 10.1 Phase 0 (Measurement)

PR 1 と同じ性質。実装変更ではなく実測値の確定が成果物。**推定との一致は
要求しない**。

- [ ] byok-ai-chat warm Desktop click → first paint で `SURFACE-TIMING` が
  出る
- [ ] cold / warm-reuse 各 5 回計測し、stage 別の median と p90 を取得
- [ ] §1.1 の hypothesis 表を実測値で書き換える
- [ ] top 2 bottlenecks を §1.1 末尾に追記
- [ ] その実測に基づき Phase 1–3 の優先順位を **再決定** する（推定どおりで
  あることは要求しない）

### 10.2 Phase 1 (Subprocess elimination)

- [ ] reuse-eligible session が disk にあるとき、`subprocess_spawn` stage
  が出ない（fast path が効いている）
- [ ] reuse-ineligible のとき、subprocess fallback が走り Phase 0 cold path
  と同じ結果を返す
- [ ] §3.2 の第一候補 / 第二候補のどちらを採用したか実装と RFC §13 / TODO
  に明記
- [ ] session record 破損 / 5 条件 fail / TTL 切れ で crash しない
- [ ] Phase 0 baseline と比較して `subprocess_spawn` + `envelope_parse` の
  reuse path 合計が 0 に近い（ms 単位）

### 10.3 Phase 2A (Surface Retention) — **本命**

- [ ] 同じ capsule/session を pane close → 5 分以内に再 open すると
  `result_kind=materialized-surface`
- [ ] retention hit 時:
  - `webview_create_*` stage が出ない
  - `navigation_finished` stage が出ない（または同 document の re-attach のみ
    で < 10ms）
  - **click → first_visible_signal が < 100ms**
- [ ] retention TTL 切れ後の再 open は新規生成パスに倒れる（Phase 0 cold
  path と同等の時間）
- [ ] cross-partition での誤 reuse が発生しない（unit test）
- [ ] `ato app session stop` 後の再 open は新規生成（retention drop）
- [ ] LRU 上限超過時に oldest が destroy される（unit test）

### 10.4 Phase 2B (WebView Pool spike — optional)

`Phase 0 + Phase 2A` 完了後に判定:

- retention miss 経路の `webview_create_*` が依然問題サイズなら spike 着手
- 問題サイズでなければ Phase 2B 自体を v1 に降格（RFC §13 に記録）

spike を実施する場合の acceptance:

- [ ] Wry / WKWebView の partition / scheme rebind 制約を実機で確認、
  RFC §13 に記録
- [ ] rebind 不可 → `WebContext` のみ pool 化、または Phase 2B を v1 に降格
- [ ] rebind 可 → pool hit 時 `webview_create` < 10ms、cross-capsule reuse
  不可の unit test

### 10.5 Phase 3 (Native Overlay + Prefetch)

#### 3a Native overlay (本命):

- [ ] retention miss 時、click → overlay visible が < 100ms
- [ ] retention hit 時、overlay は出ない（または 1 frame で hide）
- [ ] overlay dismiss が `navigation_finished` または `first_paint_signal`
  に同期

#### 3b Prefetch (best-effort):

- [ ] `prefetch_started` / `prefetch_completed` stage が出る
- [ ] prefetch 失敗で表示が壊れない
- [ ] loopback 以外の host へ送信していない（unit test）
- [ ] credential / cookie が inject されていない（unit test）
- [ ] 実測で効果がなければ Phase 3 から外す決定を §13 に記録

### 10.6 全 Phase 後の総合 (シナリオ別)

| Scenario | Hard gate (v0) | Aspirational |
|---|---|---|
| Phase 2A retention hit (再 open) | click → visible **< 100ms** | click → interactive < 200ms |
| warm reuse but retention miss | regression なし、Phase 0 cold path 比で改善 | first visible < 1s best-effort |
| cold Desktop 初回 click | regression ±100ms 以内 | — |
| first interactive (全シナリオ) | Phase 0 で測定、hard gate は Phase 0 後に決定 | warm < 1s |

`first_interactive < 1s` は **aspirational**（最終目標）であり v0 の hard
gate ではない。Phase 0 で hydration コストの実測値を取った上で、Phase 1–3
完了後に gate を再設定する。

## 11. 移行パス

各 Phase は独立に shippable。

- Phase 0 (PR 3): 計測基盤のみ。挙動変更なし。低リスク
- Phase 1 (PR 4): direct read fast path。subprocess fallback があるので
  互換性安全。第一候補（shared validation crate）と第二候補（record-only
  validation）の選択は実装着手時に決める
- Phase 2A (PR 5): retention。retention miss fallback があるので互換性安全。
  cross-partition reuse を構造的に禁止しているので security regression なし
- Phase 2B (PR 5b): pool spike。Phase 0 + 2A の数値次第で実装するか v1 に
  降格するかを決める
- Phase 3 (PR 6): native overlay + prefetch。両方とも best-effort で fallback
  あり

各 Phase の前に:
- 前 Phase の acceptance を満たしているか確認
- 数値で次の Phase の必要性を判断（Phase 1 の効果が小さければ Phase 2A を
  優先する等の動的優先順位付けを許容）

## 12. オープンクエスチョン

- **Phase 1 の 5 条件 validation を共有 crate 化するか**: §3.2 の第一候補
  （`ato-session-core` 新 crate）と第二候補（Desktop 側 record-only
  validation）の決定。第一候補の方が drift しにくいが crate boundary 設計が
  必要。実装着手時に判断
- **Phase 2A retention の TTL / size 上限の妥当値**: v0 では 5 分 / 8 entries
  を仮置き。Phase 0 / Phase 2A 完了後の運用観察で調整（hidden WebView の
  GPU メモリ実測値 / ユーザの操作パターンに依存）
- **Phase 2A の hidden WebView が network を保ち続けることの是非**: Next.js
  の SSE / websocket 接続が retention 中も生きていると capsule 側が想定外
  の挙動（auto-refresh 等）を取る可能性。capsule 側 spec で hidden 時の挙動
  を declare する仕組みが要るかは v1 議論
- **Phase 2B の partition_id hot-rebind が Wry で許可されない場合の代替**:
  pool 内 WebView を generic profile で起動し、capsule への bind 時に preload
  を inject し直す形は v0.1 案だったが §9.1 で禁止に変更。代替は
  `WebContext` のみ pool 化（実装コスト低、効果も低）
- **Phase 3 の prefetch を CLI 側に移すべきか**: `ato app session start` の
  内部で prefetch すれば Desktop 改修不要。ただし prefetch のキャンセル
  semantics（user が click を取り消した場合）が CLI 側では取れない。Desktop
  が click context を持つので Desktop 側に置く判断が今のところ妥当
- **GPUI paint pipeline と WebView の同期**: `first_paint_signal` を取る
  ためには GPUI 側 hook と WebView 内 `PerformanceObserver('paint')` →
  bridge 経路の両方が要る。Phase 0 で実機調査
- **`first_interactive` を capsule 側から expose する API**: capsule の
  hydrate 完了を Ato が知る contract（`window.__ATO_INTERACTIVE = true`
  postMessage 等）が要るかは v1 議論。v0 では best-effort

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
  `WebViewManager::sync_from_state`。Phase 2A retention の挿入位置
- `apps/ato/crates/ato-desktop/src/webview.rs:363` (`WebViewBuilder`) —
  Phase 2B pool spike の参照点
- `apps/ato/crates/ato-desktop/CLAUDE.md` — Desktop 内部のアーキテクチャ
  ガイド。Phase 2A の retention lifecycle 設計時に参照
- `apps/ato/crates/ato-cli/src/application/launch_materialization.rs` —
  CLI 側の 5 条件 validation。Phase 1 で shared crate 化するか subprocess
  化するかの議論対象 (§3.2)
- `apps/ato/crates/ato-cli/src/app_control/session.rs:951` — `session_root()`
  の env 解決ルール (`ATO_DESKTOP_SESSION_ROOT`)。Phase 1 の direct read は
  これと同じ env を尊重する
