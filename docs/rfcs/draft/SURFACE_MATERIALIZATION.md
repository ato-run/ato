# 📄 Desktop Surface Materialization Specification

**Document ID:** `SURFACE_MATERIALIZATION`
**Status:** Draft v0.4 (Phase 0 + Phase 1 完了)
**Target:** ato-desktop v0.5.x
**Last Updated:** 2026-04-29

> **v0.4 changes from v0.3** (PR 4A 完了後パッチ):
> - **Phase 1 完了**: PR 4A.0 (`ato-session-core` crate + atomic write) +
>   PR 4A.1 (Desktop session-record fast path) + PR 4A.2 (RFC 固着 +
>   fallback unit tests) を投入。warm rapid re-click が 5474 ms →
>   ~167 ms (§1.1 "Phase 1 measured result")。
> - §10.2 を observed として全て `[x]` 化、実時間 hard gate は CI に
>   入れず RFC 観察事実として固定。
> - §12 にオープンクエスチョン 2 件を Phase 2A blocker として昇格:
>   `Surface close ≠ Session stop` の UX contract patch、`partition_id`
>   生成器の統一。
> - "rapid re-click" と "close → re-click" の挙動差を §1.1 に確定仕様
>   として明記 (Phase 1 では前者のみ改善、後者は Phase 2A 領域)。
> - Phase 1 は **shared crate (`ato-session-core`) + record-only
>   validation + full launch-session fast path** で確定。30 秒 TTL は
>   不採用 (record + 5 条件で同等の鮮度確認可)。

> **v0.3 changes from v0.2** (pre-PR-3 patch):
> - Subprocess split: Desktop hot path runs **two** CLI subprocesses
>   (`ato app resolve` + `ato app session start`). Phase 0 measures them
>   separately; Phase 1 has two distinct fast paths (§3.1, §3.2, §5.1).
> - `write_session_record` is currently a non-atomic `fs::write`. Phase
>   1 lists making it `tmp + rename` as a CLI prerequisite (§9.4).
> - `first_visible_signal` promoted to a defined Phase 0 stage; this is
>   the v0 hard-gate metric (§5.1, §10.6).
> - `navigation_finished` weakened to **required-if-supported** so
>   PR 3 doesn't block on Wry callback availability (§5.1).
> - Phase 2A precondition: verify `partition_id` is stable across pane
>   close → reopen before retention can hit (§3.3).
> - `RetainedSurface` gains `last_pane_id` / `route_key` /
>   `retained_reason` for ownership tracking (§4).
> - Retention is **only** allowed on pane-close / route-deactivation;
>   session-stop, route-changed-to-different-capsule, bridge-change all
>   force destroy (§3.3).
> - Phase 1 record-only TTL: 30 s (explicit) (§3.2).
> - Phase 3 prefetch ships behind `ATO_SURFACE_PREFETCH=1` feature flag,
>   default off until measurement (§3.4).
> - §1.3 storage column: "in-process retention table; optional pool".

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

> **Status (2026-04-29)**: 上記 hypothesis ブロックは Phase 0 計測完了後の
> **superseded** 扱い。実測値を以下に置く。設計判断は実測値ベースで行う。

#### Phase 0 measured result (PR 3 — 2026-04-29)

`capsule://ato.run/koh0920/byok-ai-chat` (byok-ai-chat 標準) に対し
`ATO_SURFACE_TIMING=1 ATO_PHASE_TIMING=1` 付き release ビルドで実機計測。

| Stage | Cold (1 回目) | Warm (2 回目, 同一 session 再利用) |
|---|---:|---:|
| `resolve_subprocess` | 2150 ms | 1335 ms |
| `session_start_subprocess` | 5836 ms | 3982 ms |
| `webview_create_end` (`build_as_child` の実コスト) | 8 ms | 5 ms |
| navigation start ↔ finished | ~36 ms | ~34 ms |
| **`navigation_finished since click` (≒ first_visible_signal)** | **8230 ms** | **5474 ms** |

注: `session_start_subprocess` は cold/warm とも同じ
`session_id=ato-desktop-session-93060` を返した — **CLI 内のセッション再利用は
機能している**。warm でも 4 秒残るのは fork + CLI 全体走行 + 再検証コスト。

#### Top 2 bottlenecks

1. **`session_start_subprocess` (warm 3982 ms / cold 5836 ms)** — fork +
   CLI 走行 + resolve 再走 + reuse 検証の合算。WebView 生成や navigation
   とは独立した CLI 副作用コスト。
2. **`resolve_subprocess` (warm 1335 ms / cold 2150 ms)** — capsule://
   ハンドル → ato.run レジストリ解決のラウンドトリップ。CLI 側のキャッシュで
   削減すべき領域。

WebView 生成 (5–8 ms) と navigation (~34 ms) は合計 ~50 ms。**仮説で支配項
としていた `JS bundle exec / hydrate ~1200ms` / `WebView 生成 ~300ms` /
`Next.js SSR ~300ms` はいずれも非支配**。実体は 100% に近い割合で CLI
subprocess。

#### `partition_id` 安定性 (§3.3 precondition の検証結果)

`SurfaceExtras` debug fields (PR 3) で 2 連続クリックの partition_id を
比較したところ **不安定** だった:

- 1 回目: `capsule:__ato.run_koh0920_byok-ai-chat`
- 2 回目: `capsule---ato-run-koh0920-byok-ai-chat`

`(session_id, partition_id)` を retention key にする §2.2 案は **現状の
partition_id 生成では維持できない**。Phase 2A は partition_id 安定化
(別 PR) を前提条件とする。

#### 実装優先順位の再定義 (Phase 0 実測を受けて)

実測前は Phase 2A (Surface Retention) を v0 ヘッドラインに置いていたが、
WebView ライフサイクル合計が ~50 ms しかない以上 retention の上限効果は
~50 ms。一方 CLI subprocess は warm で 5.3 秒残る。

→ **Phase 1 (subprocess elimination) を v0 最優先に昇格**、Phase 2A は
partition_id 安定化と Phase 1 の効果実測の後に再評価する。

| 旧優先度 | 新優先度 | 根拠 |
|---|---|---|
| Phase 2A: Surface Retention (PR 5) | **Phase 1: Subprocess elimination (PR 4A)** | 実測で CLI subprocess が ~97% 支配 |
| Phase 1: Subprocess elimination (PR 4) | Registry resolve cache (CLI 側 PR 5) | warm `resolve_subprocess` 1335 ms |
| Phase 3: Native overlay + Prefetch | partition_id 安定化 (PR 6) | retention 前提条件 (§3.3) |
| Phase 2B: Pool spike | Phase 2A: Retention | partition_id 安定化後に再評価 |

PR 4 の実装方針は **「`ato-cli` を Desktop に直リンク」ではなく**
record-only fast path から始める:

- **PR 4A — Desktop session-record fast path**: warm では
  `~/.ato/apps/ato-desktop/sessions/*.json` を直接読み、PID alive +
  healthcheck だけで `CapsuleLaunchSession` を再構築。`resolve_subprocess`
  / `session_start_subprocess` を呼ばない。失敗時は subprocess fallback。
- **PR 4B — shared `ato-session-core` crate (4A で不足する場合のみ)**:
  session reuse 5 条件のうち launch_digest 再計算など record だけで
  代替できないものが残った場合に切り出し。CLI と Desktop 両方が同じ
  crate を呼ぶ。CLI command runner 全体ではなく副作用の少ない
  validation helper のみが対象。

判断根拠: `ato-cli` 全体を Desktop に直リンクすると clap routing /
tracing init / tokio runtime / stdout 契約 / process exit semantics の
境界を壊す。Desktop が必要としているのは "副作用の少ない app-control
library" であって CLI 全体ではない。PR 4A の record-only fast path で
warm 経路の subprocess を 0 にできるなら crate 切り出しは不要。

> **Ato philosophy**: warm path では resolve/start を高速化するのではなく
> **そもそも呼ばない**。Build Materialization が `cargo build` を
> 高速化するのではなく skip させたのと同じ哲学を Surface に適用する。

| 問題 | 具体 |
|---|---|
| ato CLI subprocess が常に必要 (実測支配項) | `orchestrator::resolve_and_start_capsule`（orchestrator.rs:377）が `ato app resolve` + `ato app session start` を spawn。warm でも合計 5317 ms |
| 同一 capsule への click ごとに WebView を再生成 | `WebViewManager::sync_from_state`（webview.rs:376）で pane の partition_id が変わるたびに WebView を tear down + 新規生成。pool / preload なし。実測コストは 5–8 ms と支配項ではないが、Phase 2A retention の対象 |
| `partition_id` 不安定 | 同一 handle に対し別エンコーディング (`capsule:__...` / `capsule---...-`) が混在。retention key 不適合。Phase 2A 前提 |
| 初回 fetch まで surface が空 | WebView は about:blank → URL 切替 → SSR 待ち の直列。実測 navigation ~34 ms と短いが体感の "空白時間" は subprocess 5 秒 + navigation 0.05 秒 |
| URL prefetch 未実装 | session record から `local_url` が取れた瞬間に並行 fetch を開始すれば SSR を warm にできるが、現在は WebView 生成後に初めて HTTP が走る。Phase 1 後の効果再評価が必要 |

#### Phase 1 measured result (PR 4A.1 — 2026-04-29)

PR 4A.1 (Desktop session-record fast path) を投入後、同じ
`capsule://ato.run/koh0920/byok-ai-chat` ハンドルで再計測した結果:

| Click | 経路 | `resolve_subprocess` | `session_start_subprocess` | `session_record_lookup` | `session_record_validate` | **total since_click** |
|---|---|---:|---:|---:|---:|---:|
| 1 (cold, 空 record) | fallback | 1447 ms | 6835 ms | 0 ms | (発火せず) | **8534 ms** |
| 2 (warm, 同一 process) | **fast path** | — | — | 1 ms | 10 ms | **170 ms** |
| 3 (warm, 同一 process) | **fast path** | — | — | 0 ms | 11 ms | **163 ms** |
| 4 (close → re-click) | fallback | 1780 ms | 4058 ms | 0 ms | (発火せず) | 5994 ms |

**主要な改善** (warm 連続 click):

```text
Phase 0 baseline (PR 4A 前):  5474 ms
PR 4A.1 fast path hit:        163–170 ms (中央値 ~167 ms)
削減:                         ~5307 ms (97%, ~33×)
```

**内訳**:

```text
session_record_lookup:   0–1 ms   (dir scan + JSON parse)
session_record_validate: 10–11 ms (大半は HTTP healthcheck)
webview_create:          5–16 ms
navigation_finished:     ~110 ms after webview_create
```

**Phase 0 の仮説で支配項としていた項目** (`JS bundle exec / hydrate
~1200ms` / `WebView 生成 ~300ms` / `Next.js SSR ~300ms`) は **すべて
非支配** が確定。実体は 100% に近い割合で CLI subprocess だった、という
Phase 0 結論が定量的にも追認された。

#### "rapid re-click" と "close → re-click" の挙動差 (確定仕様)

実測で 2 つの hit 条件が分離した:

| シナリオ | 結果 | 理由 |
|---|---|---|
| **rapid re-click** (同じ pane handle を立て続けに表示) | fast path hit (~167 ms) | session record が disk に残っており、PID + start_time + healthcheck が全て通る |
| **close → re-click** (pane を閉じてから同じ handle を再オープン) | fallback (~6 秒) | 現行の `WebViewManager::stop_launched_session` が pane close 時に `ato app session stop` を呼び session record を削除する。次クリック時は record 不在 → fallback |

**Phase 1 はあくまで "session record が生きている間の" 体感を改善する**。
pane close 時に session を停止する現在の振る舞い自体は変えない。
"close → re-click" を fast path に乗せるには **Surface close ≠ Session
stop** の semantic 変更が必要で、それは Phase 2A retention の領域
(§3.3 / §12 を参照)。

この区別は意図的:

- 単純な最適化 (record TTL 延長など) で済む話ではなく、UX contract
  (pane を閉じる = session を止める / surface だけ閉じる の選択) を
  決めることが先。
- Phase 1 までは "ユーザーが pane を閉じた = 終了したい" という現
  契約を維持し、Phase 2A で改めて議論する。

#### PR 4A の構成 (確定)

実装は 3 段階に分割し、計測結果を踏まえて crate 分離まで完了:

| PR | 目的 | 成果 |
|---|---|---|
| **PR 4A.0** | shared `ato-session-core` crate 新設 + atomic write 化 | `StoredSessionInfo` / display structs / `session_root` / `read_session_records` / `pid_is_alive` / `process_start_time_unix_ms` / `http_get_ok` / `validate_record_only` を CLI から切り出し。`write_session_record_atomic` (temp + rename) で §9.4 prerequisite を解消。22 unit tests green |
| **PR 4A.1** | Desktop fast path 実装 | `try_session_record_fast_path` を `resolve_and_start_capsule` の冒頭に挿入。SURFACE-TIMING に `session_record_lookup` / `session_record_validate` stage 追加。fallback path は debug! ログのみで silent 維持 |
| **PR 4A.2** | 計測固着 + fallback correctness 担保 | RFC §1.1 / §10.2 に実測値追記、orchestrator に 10 fallback unit tests (record missing / schema=1 / digest missing / pid dead / start_time mismatch / healthcheck fail / handle alias / corrupt JSON / nonexistent root / valid record builds session)。実機計測の hard gate は CI に入れない (flaky) |

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
| Storage | `.ato/state/materializations.json` (project-local) | `~/.ato/apps/<pkg>/sessions/` (host-local) | in-process retention table; optional WebView/WebContext pool (Phase 2B); persistence は v1+ |
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

Desktop の hot path には **2 つの CLI subprocess** がある（`orchestrator.rs:386`
`resolve_capsule(handle)` → `ato app resolve --json` と `:387`
`start_capsule(...)` → `ato app session start --json`）。Phase 1 の fast path
はこの 2 つを別々に最適化する余地があるため、Phase 0 でも別々に計測する。

| Stage | 必須 / Optional | 取り方 |
|---|---|---|
| `click_start` | 必須 | GPUI の click handler 入口 |
| `resolve_subprocess` | 必須 (subprocess 経路時) | `ato app resolve --json` の `run_ato_json` 開始 → exec 戻り |
| `envelope_parse_resolve` | 必須 | resolve 結果の `serde_json::from_str` |
| `session_start_subprocess` | 必須 (subprocess 経路時) | `ato app session start --json` の `run_ato_json` 開始 → exec 戻り |
| `envelope_parse_session_start` | 必須 | session start 結果の `serde_json::from_str` |
| `session_resolved` | 必須 | 両 envelope 取得完了（fast path 時は record 直読の戻り） |
| `build_launch_session` | 必須 | `build_launch_session` 完了（resolved + started のマージ） |
| `pane_state_build` | 必須 | `AppState` への pane 反映（`sync_from_state` への signal 含む） |
| `webview_create_start` / `_end` | 必須 | Wry `WebViewBuilder::build` の前後 |
| `navigation_start` | 必須 | `webview.load_url(local_url)` 呼び出し |
| `navigation_finished` | **必須 if supported, otherwise best-effort** | Wry の navigation callback。取れない platform / version では出さず "navigation finish stage unavailable" と doc に記録 |
| `first_dom_content_loaded` | 可能なら | injected script の `DOMContentLoaded` |
| `first_paint_signal` | best-effort | injected `PerformanceObserver('paint')` → host_bridge 経由 postMessage |
| **`first_visible_signal`** | **必須** | retention hit: retained WebView を visible 化した時点。retention miss: native overlay の first paint または `navigation_finished` のいずれか早い方。**v0 hard-gate metric (§10.6)** |
| `first_interactive_signal` | optional | app-specific marker (Next.js hydrate 完了 hook 等)。capsule 側協力なしには取れないことを許容 |
| `total` | 必須 | click_start からの累計 |

`first_visible_signal` の定義 — ユーザが「pane に何か見えた」と認識する最も早い瞬間:

```
Retention hit:
  retained WebView has been attached and made visible.

Retention miss:
  whichever fires first of:
    - native overlay (Phase 3a) has produced its first paint
    - underlying WebView has fired first_paint_signal / navigation_finished
```

これは `first_interactive` より早い段階で取れ、retention の効果を直接的に
評価できるため v0 の hard gate に採用する（§10.6）。

**Debug extras**: 各 SURFACE-TIMING 行には `session_id` / `partition_id` /
`route_key` を debug field として出すと、retention 設計時に partition_id 安定性
（§3.3 precondition）の確認に使える。

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

**前提**: Phase 0 の実測で `resolve_subprocess` + `session_start_subprocess`
の合計が有意に大きいことを確認してから着手（推定 ~150ms × 2 = ~300ms 上限）。
50ms 未満であれば Phase 2A retention を優先し、Phase 1 を後送りする。

**Two fast paths**:

Phase 1 が削れる subprocess は概念的に 2 つあり、難易度と効果が異なる:

| Fast path | 削る対象 | 難易度 | 必要な情報 |
|---|---|---|---|
| **Session-start fast path** | `ato app session start` のみ | 中 | session record (5 条件 validation pass) |
| **Full launch-session fast path** | `ato app resolve` + `ato app session start` 両方 | 高 | session record + resolved metadata cache (handle / trust_state / source / snapshot) |

PR 3 (Phase 0) で **両方の subprocess を別々に計測** し、どちらの fast path
が割に合うかを実測根拠で判断する。RFC では両方を選択肢として提示し、PR 4
着手時の実測で 1 本に絞る。

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

RFC v0.3 では this を hard limitation として明示する。manifest / source
change が予想されるシナリオ（`ato run` から開発中など）では subprocess
fallback に倒すための **fast-path TTL = 30 秒** を fast-path 結果に持たせる
（cache record の `validated_at` から 30 秒経過で fast-path を強制 miss、
次の操作で subprocess fallback が走る）。第一候補（shared crate）採用時は
launch_digest の current 値を計算できるので、この TTL は不要 or 大幅に
延長できる。

#### 避ける案: Desktop 独自 LaunchSpec canonicalization

drift しやすく、CLI と Desktop の bug fix が必ず双方で必要になる。**v0 では
やらない**（明示的に Out of scope）。

**Prerequisite (CLI 側)**:

現行 `app_control/session.rs::write_session_record` は
`fs::write(&path, ...)` の単純書き込みであり、atomic ではない。Phase 1 で
Desktop が disk を直読する場合、CLI が書き込み中の partial JSON を読んで
parse エラーを起こしうる（fallback で subprocess に倒れるので機能的には
安全だが、race を頻発させると Phase 1 の効果が消える）。

**Phase 1 着手前に CLI 側を temp+rename atomic write に直す**:

```rust
// session.rs::write_session_record を:
let tmp = path.with_extension("json.tmp");
fs::write(&tmp, serialized)?;
fs::rename(&tmp, &path)?;
```

これは BUILD_MATERIALIZATION の `materializations.json` save 経路と同じ
パターン (`build_materialization::save_state`)。1 commit で済む小変更。
PR 4 の最初の prep として CLI 側で landing。

**変更点 (Desktop 側)**:

- `orchestrator.rs:386–388` `resolve_capsule` / `start_capsule` /
  `build_launch_session` の前段に fast-path branch を追加（別関数
  `try_resolve_session_from_record` 等）
- record 直読 + validation。fail なら **必ず** subprocess fallback
- subprocess fallback 経路は現状を変更しない
- `SURFACE-TIMING` で `resolve_subprocess` / `session_start_subprocess` が
  出ない / 出る を別々に確認できる
- session-start fast path 採用なら `resolve_subprocess` のみ残る、
  full launch-session fast path 採用なら両方消える

**Out of scope**:

- 失敗時 fallback の挙動変更
- `--respawn` 等の flag（CLI v1 待ち）
- daemon 化（別 RFC）
- manifest / source change の自前検知（第二候補採用時は明示的に limitation）

**Acceptance**:

- [ ] CLI 側 `write_session_record` が temp+rename atomic に変更済み
- [ ] Phase 0 baseline と比較して、採用した fast path の対象 subprocess
  stage が reuse-eligible 経路で 0 に近い
- [ ] session-start fast path / full launch-session fast path のどちらを
  採用したか RFC §13 / TODO に明記
- [ ] §3.2 第一候補 (shared crate) / 第二候補 (record-only + 30s TTL) の
  どちらを採用したか §13 / TODO に明記
- [ ] record 破損 / 短読 / 5 条件 fail / TTL 切れ で必ず subprocess fallback
- [ ] Phase 0 の fallback path 数値が劣化していない（regression なし）

### Phase 2A: Session-Keyed Surface Retention (PR 5, 本命)

**目的**: 同じ `(session_id, partition_id)` への再 click で WebView 生成 +
navigation + JS bundle exec + hydrate を **全部消す**。本 RFC v0 の最大の
optimization。

**Precondition (Phase 2A 着手前に検証)**:

retention key は `(session_id, partition_id)`。Desktop 側で **pane close →
reopen 時に `partition_id` が安定しているか** を確認しないと retention は
永遠に hit しない。Phase 0 (PR 3) で SURFACE-TIMING の debug extras に
`session_id` / `partition_id` / `route_key` を含め、close/reopen サイクルで
これらが変わらないことを実測する。

不安定だった場合の代替設計（Phase 2A の中で選択）:

1. **Retained partition を権威に**: retention 時の `partition_id` を保持し、
   reopen 時に Desktop 側が retained partition を使うよう pane state を
   調整する（pane が新 partition を要求しても retained を優先）
2. **Stable route key で keying**: `(session_id, route_key)` を key にし、
   retained WebView の `partition_id` を attach 時に転写する

どちらも `WebViewManager::sync_from_state` の diff モデルへの介入が必要。
Phase 0 計測で安定と分かった場合のみ単純な
`(session_id, partition_id)` keying で済む。

**変更**:

- `WebViewManager` に `RetentionTable: HashMap<SurfaceKey, RetainedSurface>`
  を追加
- pane close / route deactivation で WebView を destroy する箇所で:
  - retention 条件 (下記) を満たす → destroy せず hidden 化して retention
    table に移す
  - 満たさない → 既存経路で destroy
- pane click / route activation で新規 `WebView` を作ろうとする箇所で:
  - retention table に hit があり §2.2 の 5 条件すべて pass → attach +
    reveal だけで完了
  - miss → 既存経路で新規生成

**Retain 可能な destroy 理由 (重要)**:

`sync_from_state` が WebView を destroy する原因は複数あり、retain 可能なのは
**一部のみ**。誤って retain すると security / lifecycle 違反になる:

| Destroy 理由 | Retain 可? |
|---|---|
| pane が画面から閉じられた | ✅ Yes |
| route が一時的に inactive になった (tab 切替等) | ✅ Yes |
| route が **別 capsule** に変わった | ❌ No (新規生成必須) |
| underlying session が `ato app session stop` で明示的に停止 | ❌ No (retention drop) |
| WebView の bridge / security profile が変わった | ❌ No (新規生成) |
| WebView の render プロセスが crash | ❌ No (drop) |
| Desktop 終了時 | ❌ No (Drop で全 retention destroy) |

実装上は destroy トリガを enum で分類し、retain 可な variant のみ retention
ルートに分岐する。

**Retention の TTL / LRU**:

- v0 default TTL: **5 分**（idle 時間。最後に attach 解除された時点から）
- 上限: 同時 retain 数 **8**（OS の WebView 上限・GPU メモリを考慮した
  保守値）。超過時は LRU で oldest を destroy
- 上限超過時に新規 retain を試みた場合、retention をスキップして単純
  destroy（regression にはならない）

**設計上の難所** (Phase 2A 固有):

- **`partition_id` 安定性**: 上記 precondition 参照
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

- [ ] Phase 0 で `partition_id` 安定性を確認済み（§3.1 debug extras）
- [ ] 同じ capsule/session を pane close → 5 分以内に再 open すると
  `result_kind=materialized-surface`
- [ ] `webview_create` stage が出ない（または < 10ms）
- [ ] `navigation_finished` stage が出ない（または既存 document を再表示
  するだけで < 10ms）
- [ ] click → first_visible_signal が **< 100ms**
- [ ] retention TTL 切れ後の再 open は新規生成パスに倒れる（cold path と
  同じ数値）
- [ ] cross-partition での誤 reuse が発生しない（§9.1）
- [ ] route が別 capsule に切替わったときに retention が drop される（unit test）
- [ ] `ato app session stop` 後の再 open は新規生成（retention drop されている）
- [ ] Desktop 終了時に全 retention が Drop で destroy される

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

#### 3b. Surface Prefetch (best-effort, feature-flagged)

**変更**:

- session envelope の `local_url` が判明した直後に `tokio::spawn` で
  `http::Client::get(local_url)` を fire-and-forget
- レスポンス body は **破棄**（warming 目的のみ）
- credential / cookie は **inject しない**
- 対象は **loopback (`127.0.0.1` / `localhost`) のみ**。それ以外の host へは
  fetch しない
- **Default off**。`ATO_SURFACE_PREFETCH=1` env で opt-in。Phase 0 / Phase 3
  実測で効果と安全性を確認してから default on を検討する

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

prefetch は **best-effort + feature-flagged**。default off を選んだ理由は
GET side-effect の risk を完全には排除できないため。capsule 側のセマンティ
クスを変えずに勝手に GET するのは保守的でない判断であるべき。Phase 3 完了
後に実測で:

- 効果あり → next iteration で `[surface] prefetch = true` の opt-in
  declaration を v1+ で導入
- 効果なし → `ATO_SURFACE_PREFETCH=1` flag だけ残して実装は維持、または
  feature 削除を §13 に記録

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
    webview: wry::WebView,             // hidden, not destroyed
    web_context: wry::WebContext,
    local_url: String,                 // retention 時の URL（変わったら drop）
    retained_at: Instant,              // TTL 計算の起点
    partition_id: String,
    /// 直前に attach されていた pane の id。reopen 時に同じ pane への
    /// re-attach か別 pane への attach かを診断する目的。診断のみで
    /// reuse 判定には使わない。
    last_pane_id: Option<usize>,
    /// pane が指していた route の安定 key（`GuestRoute` の textual form
    /// など）。`partition_id` が pane-instance-scoped で揺れる場合の
    /// fallback identity に使える（§3.3 precondition の代替設計 2）。
    route_key: String,
    /// Retention に至った理由。`sync_from_state` の destroy トリガを
    /// retain 可な variant のみに絞るための tag（§3.3 retain 可表）。
    retained_reason: RetainedReason,
}

#[derive(Debug, Clone, Copy)]
enum RetainedReason {
    /// pane を画面から閉じた
    PaneClosed,
    /// pane は残っているが route が一時的に inactive
    RouteDeactivated,
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
書いている最中**を読む可能性がある。

**現状 (v0.3 時点)**: `app_control/session.rs::write_session_record` は
単純な `fs::write(&path, ...)` であり、**atomic ではない**。partial JSON を
読んだ Desktop は `serde_json::from_slice` で parse error を返す。

**Phase 1 prerequisite — CLI 側 atomic write**:

Phase 1 着手前に CLI 側を `temp + rename` 書き込みに直す。これは
BUILD_MATERIALIZATION の `materializations.json` save 経路 (PR `0c32ea4`
の `build_materialization::save_state`) と同じパターン。1 commit で済む
小変更で、Phase 1 / 将来の retention persistence / 他の session record 直読
ユースケースの基盤になる。

```rust
fn write_session_record(root: &Path, session: &StoredSessionInfo) -> Result<()> {
    let path = root.join(format!("{}.json", session.session_id));
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(session)?)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}
```

**Defense in depth — Desktop 側の fallback**:

CLI 側 atomic write が landing しても、Desktop は依然として soft-miss
fallback を持つ:

- `serde_json::from_slice` 失敗 → subprocess fallback
- short read / partial JSON / 必須 field 欠落 → subprocess fallback
- record の `schema_version` が unset / `< 2` → subprocess fallback
- 5 条件 validation のいずれかが fail → subprocess fallback
- (record-only validation 採用時) 30 秒 TTL 切れ → subprocess fallback

「parse error は失敗」「parse 通っても validation で失敗」の二段で守る。

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

- [x] byok-ai-chat warm Desktop click → first_visible_signal で
  `SURFACE-TIMING` が出る (2026-04-29)
- [ ] cold / warm-reuse 各 5 回計測し、stage 別の median と p90 を取得
  (現状は 1 サンプル ずつ。median/p90 取得は PR 4A 後の比較計測時に実施)
- [x] **`resolve_subprocess` と `session_start_subprocess` が別々に計測されて
  いる** (Phase 1 の fast path 選択に必須)
- [x] **`first_visible_signal` が必ず出る** (v0 hard-gate metric;
  `since_click_ms` 込みで emit、`SURFACE-TIMING total` line も追加)
- [ ] `navigation_finished` が取れる platform / 取れない platform で挙動が
  分岐 (取れない場合は出さない、doc に limitation を記録)
- [x] **debug extras `session_id` / `partition_id` / `route_key` が出ている**
  (Phase 2A precondition の partition_id 安定性確認に使用) — 検証結果:
  **partition_id は不安定** (§1.1 参照)
- [x] §1.1 の hypothesis 表を実測値で書き換える (§1.1 "Phase 0 measured
  result" 節)
- [x] top 2 bottlenecks を §1.1 末尾に追記
  (`session_start_subprocess` / `resolve_subprocess`)
- [x] その実測に基づき Phase 1–3 の優先順位を **再決定** する — Phase 1 を
  最優先に昇格、Phase 2A は partition_id 安定化後に再評価 (§1.1 末尾)

### 10.2 Phase 1 (Subprocess elimination)

PR 4A.0 + 4A.1 + 4A.2 で達成 (§1.1 "Phase 1 measured result" 節参照)。
チェックボックスは observed = `[x]` で固定済み。"hard gate" を CI に
入れているのは挙動 acceptance のみ — 実時間 (warm < 500 ms など) は
flaky のため RFC 観察事実として残し CI ゲートにはしていない。

- [x] **CLI 側 prerequisite**: `app_control/session.rs::write_session_record`
  が temp+rename atomic write に変更済み（§9.4 / `ato_session_core::write_session_record_atomic`）
- [x] 採用した fast path (Desktop session-record full launch-session
  fast path) が reuse-eligible 経路で `resolve_subprocess` /
  `session_start_subprocess` の SURFACE-TIMING stage を emit しない
  (実機計測で確認: clicks 2-3 で両 stage 不在)
- [x] reuse-ineligible のとき、subprocess fallback が走り Phase 0 cold
  path と同じ結果を返す (実機計測で確認: click 4 で fallback 5994 ms ≒
  cold baseline)
- [x] §3.2 採用方針を実装と RFC に明記: **shared crate (`ato-session-core`)
  + record-only validation の組み合わせ**。30 秒 TTL は v0 では適用せず
  (record の存在 + pid + start_time + healthcheck で同等の鮮度確認が
  できる)
- [x] §3.1 採用方針を明記: **full launch-session fast path** (resolve も
  session_start も両方回避)。記録された `StoredSessionInfo` だけで
  `CapsuleLaunchSession` を完全構築できる (調査 1 で確認、PR 4A.1 で実装)
- [x] session record 破損 / parse error / short read / 5 条件 fail で
  crash しない (orchestrator `fast_path_tests` で 10 ケース検証)
- [x] Phase 0 baseline と比較して、採用した fast path が対象とする
  `*_subprocess` の合計が 0 に近い: 実測で `session_record_lookup`
  0–1 ms + `session_record_validate` 10–11 ms = 計 10–12 ms (Phase 0
  warm 5474 ms から ~5300 ms 削減)

**Observed in PR 4A.1** (RFC 内の「実時間」観察事実 — CI hard gate ではない):

- byok-ai-chat capsule:// warm rapid re-click で
  `total since_click = 163–170 ms` (中央値 ~167 ms)
- 同一 `session_id` が連続 click で再利用される
- record 削除後の再 click は cold path 同等 (~6 秒) に正しく倒れる
- partition_id は cold/warm で別表記が出続ける (Phase 2A 前提条件、
  未解決として §12 / TODO に残す)

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

- **(Phase 2A 前提) `Surface close ≠ Session stop`**: Phase 1 の実測で
  "rapid re-click" は ~167 ms に落ちたが "close → re-click" は依然 ~6
  秒。原因は現行 `WebViewManager::stop_launched_session` が pane close
  時に `ato app session stop` を呼んで session record を削除している
  ため。**Phase 2A retention に進む前に、UX contract を明文化する短い
  RFC patch が必要**:
  
  | 操作 | 提案する意味 |
  |---|---|
  | pane close | surface を閉じる。session は TTL 付きで残す (retention) |
  | explicit stop | session を停止し record も削除 |
  | app quit | policy に従い session stop or retain |
  | TTL expiry | session stop + retention cleanup |
  
  この整理が無いまま retention を実装すると "ユーザは閉じたつもりが
  hidden process が走り続ける" のような UX 問題に直結する。Phase 2A
  着手時の最初の RFC 議題。
- **(Phase 2A 前提) `partition_id` 安定化**: Phase 1 fast path は
  session_id を key にするので影響しないが、retention は §2.2 で
  `(session_id, partition_id)` を key にしている。実機計測で:
  - cold: `capsule:__ato.run_koh0920_byok-ai-chat`
  - warm: `capsule---ato-run-koh0920-byok-ai-chat`
  
  と 1 つの handle に対して 2 種の partition_id が発生している。**この
  生成器を統一しないと retention は永遠に miss する**。retention 実装
  前に解決が必要 (RFC §13 / TODO に blocker として残す)。
- **Phase 1 の 5 条件 validation を共有 crate 化するか**: §3.2 の第一候補
  （`ato-session-core` 新 crate）と第二候補（Desktop 側 record-only
  validation）の決定。**PR 4A.0 で第一候補 (`ato-session-core` 新規
  crate) を採用済**。crate boundary は CLI 全体ではなく schema +
  validation helper のみに絞り、CLI / Desktop 両方が依存する形にした
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
