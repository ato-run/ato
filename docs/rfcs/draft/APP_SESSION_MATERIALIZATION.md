# 📄 App Session Materialization Specification

**Document ID:** `APP_SESSION_MATERIALIZATION`
**Status:** Draft v0.2
**Target:** ato-cli v0.5.x
**Last Updated:** 2026-04-29

> **v0 implementation scope is narrow on purpose.** PR 2 only wires reuse
> into `ato app session start` (the Desktop launch path), with no
> destructive cleanup, no new `ato run` flags, and no strict port-owner
> verification. Each of those is a separate v1 work item once the reuse
> state machine is proven on the app-session entry point. See §5.2, §5.5,
> §8, §9.1.

## 1. 概要 (Overview)

`ato run` および `ato app session start` の Execute phase を「毎回 process を
spawn する step」から「declared launch contract から導出される **App Session**
が local state 上で materialized 済みかを確認し、missing / stale / unhealthy
のときだけ spawn する step」に再定義する。

これは [BUILD_MATERIALIZATION](BUILD_MATERIALIZATION.md) と同型の拡張である:

```
Build:   inputs + command + toolchain  →  build artifact
                                       →  .ato/state/materializations.json

Execute: launch spec + build digest + readiness contract  →  app session
                                                          →  per-session record + lookup
```

### 1.1 解決する問題 — PR 1 の実測根拠

`samples/byok-ai-chat` の warm `ato app session start --json` を sub-stage
timing で計測した結果（PR `050b323`）:

```
PHASE-TIMING phase=execute stage=prepare_session_execution elapsed_ms=0
PHASE-TIMING phase=execute stage=spawn_runtime_process    elapsed_ms=1
PHASE-TIMING phase=execute stage=wait_http_ready          elapsed_ms=876
PHASE-TIMING phase=execute stage=write_pid                elapsed_ms=0
PHASE-TIMING phase=execute stage=write_session_record     elapsed_ms=0
PHASE-TIMING phase=execute state=ok elapsed_ms=879
```

つまり Execute 879ms のうち **~99% は `wait_http_ready` (= app boot)**、
Ato 側の overhead は合計 **3–4ms** に過ぎない。

| 問題 | 具体 |
|---|---|
| 同一 launch contract に対する重複 spawn | `next start` を毎回起動して 880ms 待つ。Desktop 体感の主要因 |
| ready な session が捨てられる | 既存 PID が listen 中でも、次の `app session start` は別プロセスを起こして並走 |
| 判定ロジックの不在 | `start_session` には pid 再利用の概念が無く、毎回新規 spawn のみ |
| Execute の materialization 抽象が無い | Build / Install には CAS hit 判定があるが Execute には無い、というモデル非対称 |

### 1.2 設計方針

- **Session は phase ではなく artifact**: `Execute` は process を回す step では
  なく、declared launch contract を満たす materialized session を返す step
  と再定義する。
- **v0 は `ato app session start` のみが対象**: Desktop が呼ぶ唯一の reuse-
  beneficial entry point に集中し、reuse state machine の正しさをそこで証明
  してから他経路に拡張する（§5.2）。
- **v0 action は Reuse / Spawn の 2 値のみ — destructive cleanup なし**:
  既存 session が stale でも v0 では kill しない。Desktop が複数 window /
  tab を持つ可能性があり、勝手な kill は UX を壊す。stale ハンドリングは
  v1（§5.5）。
- **Internal caching ではなく external reuse**: PR 1 の実測で Ato 内部処理は
  既に十分速いことが確定したため、**`prepare_session_execution` のキャッシュは
  本 RFC の対象外**。最適化対象は spawn そのものを回避することのみ。
- **3 層モデルを汚さない**: 宣言（`capsule.toml`）／解決結果（`ato.lock.json`）／
  実機状態（`~/.ato/apps/<package_id>/sessions/`）を分離。
- **Cross-host reuse は対象外**: portable session contract が未定義のため、
  cross-machine reuse は先送り（§8）。
- **`materialized-session` という新 result_kind**: phase result は
  `state=ok result_kind=materialized-session` で表現する。BUILD_MATERIALIZATION
  §6.1 の値集合に追加。

### 1.3 Build Materialization との関係

両 RFC は意図的に対称な構造を持つ:

| 軸 | Build (RFC) | App Session (本 RFC) |
|---|---|---|
| Materialized 対象 | output tree (`.next` 等) | running process + bound port |
| Digest 入力 | source tree + command + toolchain | LaunchSpec + build_digest + readiness contract |
| Storage | `.ato/state/materializations.json` (project-local) | `~/.ato/apps/<package_id>/sessions/` (host-local) |
| Stale 検知 | digest mismatch + outputs missing | pid 死亡 / 起動時刻不整合 / digest mismatch / port 非占有 / healthcheck 不通 |
| Skip 後 result_kind | `materialized` | `materialized-session` |
| Force 再生成 flag | `--rebuild` | `--respawn` |
| Reuse-only flag | `--no-build` | `--require-session` (v1+, §8) |

両者を同じ pattern で実装することで「Reuse the model, not special cases」を維持。

## 2. コアコンセプト (Core Concepts)

### 2.1 App Session

App Session は次の関数として定義される:

```
session = launch_executor(LaunchSpec, ReadinessContract)
```

- `LaunchSpec`: 起動に必要な canonical な spec（§2.2）
- `ReadinessContract`: ready 判定の方法（HTTP path / interval / timeout）
- 結果: `{ pid, port, local_url, status: ready }` を返す materialized record

v0 では **`ato app session start`** がこの関数の materialization 版を呼ぶ。
record が ready かつ valid なら spawn をスキップして既存 session の envelope
を返す。`ato run` 各 mode の materialization 化は v1（§5.2）。

### 2.2 LaunchSpec と Launch Digest

LaunchSpec は次の要素から canonical 化される:

```
LaunchSpec = {
  capsule_handle:        canonical handle (e.g. github.com/owner/repo@vX),
  selected_target:       target label,
  command_argv:          exec name + args (NOT shell string),
  cwd:                   resolved working directory (relative),
  declared_port:         capsule.toml の port 宣言値,
  readiness:             { path, interval_ms, timeout_ms },
  env_policy:            include set + value digests (BUILD_MATERIALIZATION §4.3 と同方式),
  build_input_digest:    BUILD_MATERIALIZATION の input_digest,
  lock_digest:           ato.lock.json の lock_id,
  toolchain_fingerprint: BUILD_MATERIALIZATION の toolchain_fingerprint と共有,
}
```

Launch Digest:

```
launch_digest = blake3(
  schema_version_marker     ||  // "ato-app-session-v1"
  capsule_handle            ||
  selected_target           ||
  command_argv              ||
  cwd                       ||
  declared_port             ||
  readiness                 ||
  env_policy_canonical      ||
  build_input_digest        ||
  lock_digest               ||
  toolchain_fingerprint
)
```

`launch_digest` が変われば session は stale。一致なら reuse 候補。

`build_input_digest` を含めるのは重要: build outputs が変わったら同じ launch
コマンドでも違う app になるため、再 spawn を強制する。

### 2.3 Session Materialization Record

直前の成功した session start を記録する host-local state:

```
~/.ato/apps/<package_id>/sessions/desky-session-<pid>.json   (既存パスを流用)
~/.ato/apps/<package_id>/sessions/index.json                  (lookup index, v0 は optional)
~/.ato/apps/<package_id>/sessions/locks/<launch_key>.lock     (concurrent start lock, §5.4)
```

`<package_id>` は既に `ato-desktop` 等で確立されている scope。`launch_key` は
§5.4 で定義。

## 3. スキーマ定義

### 3.1 `[run]` セクション (v1+ で正式採用)

> **v0 の制約**: BUILD_MATERIALIZATION §3.0 と同じ理由で、capsule-core の
> 既存 schema との衝突回避のため `[run]` の正式採用は v1。v0 は既存の
> `run = "..."` / `port = N` / `readiness_probe` を canonical source として読む。
>
> 以下は v1 で目指す形（参考）。

```toml
[run]
# v0 の `run = "npm run start"` と互換。canonical 化されると
# command_argv = ["npm", "run", "start"] になる。
command = "npm run start"
cwd = "."
port = 3000

[run.readiness]
path = "/"
interval_ms = 25
timeout_ms = 10000

[run.session]
# Default reuse policy for this capsule. Per-invocation flags (§7) override.
reuse = "if-ready"     # "if-ready" | "always" | "never"
```

### 3.2 既存 v0.3 manifest からの canonical 化

v0 では既存フィールドから LaunchSpec を canonical 化する:

| LaunchSpec field | 取得元 |
|---|---|
| `command_argv` | `derive_launch_spec(plan)` の結果（既存） |
| `cwd` | `launch_spec.working_dir` |
| `declared_port` | `targets.<label>.port` または top-level `port` |
| `readiness.path` | `targets.<label>.readiness_probe.path` または `"/"` |
| `readiness.interval_ms` | 定数 25ms（`SESSION_READY_POLL_INTERVAL`） |
| `readiness.timeout_ms` | 10000ms（`SESSION_READY_TIMEOUT`） |
| `env_policy` | `targets.<label>.env` + capsule-level env include rules |

## 4. State Schema

### 4.1 Per-session record (既存 `StoredSessionInfo` を拡張)

既存の `desky-session-<pid>.json` schema に **3 fields を追加**する:

```json
{
  "session_id": "ato-desktop-session-64273",
  "handle": "samples/byok-ai-chat",
  "normalized_handle": "samples/byok-ai-chat",
  "pid": 64273,
  "log_path": "...",
  "manifest_path": "...",
  "target_label": "app",
  "notes": [...],
  "guest": {...},
  "web": {...},

  "launch_digest": "blake3:abcdef...",
  "process_start_time_unix_ms": 1714370800123,
  "schema_version": 2
}
```

| 新フィールド | 意味 |
|---|---|
| `launch_digest` | §2.2 で算出。reuse 一致判定に使う |
| `process_start_time_unix_ms` | OS から取得した process creation time。PID 再利用検知に必須（§9.2） |
| `schema_version` | record schema 番号。互換破壊時に bump |

既存 schema を読む場合は `schema_version` 不在 → schema=1 とみなし、
`launch_digest` 不在として扱う（reuse 不可、必ず spawn）。

### 4.2 Lookup Index (v0 は optional)

複数 session の中から `(handle, target, launch_digest)` で高速に探すための
flat array file:

```json
{
  "schema_version": 1,
  "sessions": [
    {
      "session_id": "ato-desktop-session-64273",
      "handle": "samples/byok-ai-chat",
      "target": "app",
      "launch_digest": "blake3:...",
      "pid": 64273,
      "port": 3000
    }
  ]
}
```

v0 では index 不要（per-session JSON を glob → parse → filter で十分。
session 数 < 100 の host で < 5ms）。v1 で session 数が増えるなら index 化。

### 4.3 Concurrent Start Lock

```
~/.ato/apps/<package_id>/sessions/locks/<launch_key>.lock
```

`launch_key` は **logical slot identifier** で、spec 変化に対して安定で
なければならない（同じ slot を奪い合う 2 starts を排他するのが目的）。
`launch_digest` とは異なる:

```
launch_key = blake3(
  schema_version_marker      ||  // "ato-launch-key-v1"
  identity                   ||  // remote handle / store scoped id / canonical local manifest path
  selected_target
)
```

`identity` の決定ルール:

| Source | identity に使う値 |
|---|---|
| Remote handle (e.g. `github.com/owner/repo@vX`) | `normalized_handle`（`normalize_capsule_handle` の結果） |
| Store scoped id (e.g. `publisher/slug`) | `normalized_handle` |
| Local path (`./`, `~/`, absolute path) | `manifest_path` を `std::fs::canonicalize` した結果 |

理由: local path 起動では同じ project への異なる handle 表現（`.`、相対 path、
絶対 path、symlink 経由）が衝突しないよう、canonical な manifest path で
identity を取る必要がある。

`fs2::FileExt::lock_exclusive`（既存依存）で OS-level の advisory file lock
を取る。`wait_http_ready` 中ずっと保持する（§5.4）。

## 5. 実行フロー

### 5.1 Session Start with Reuse (v0)

```text
session start entry  (ato app session start のみ)
├─ resolve handle → manifest, plan, launch_spec
├─ compute launch_digest
├─ acquire lock(launch_key)              ← §5.4
│    (lock holds across spawn + readiness + record write — §5.4 注釈)
├─ lookup existing records by (handle, target)
├─ state machine                          ← §5.3
│    Reuse:  return existing envelope (skip spawn)
│    Spawn:  spawn new, wait readiness, write new record
│            (existing stale record/process は kill しない — §5.5)
├─ release lock
└─ emit envelope
```

v1 で追加予定の action: `Replace` (kill existing + spawn new), `Fail`
(`--require-session` で reuse 不可) — §5.5 / §8。

### 5.2 Reuse Policy by Entry Point

**v0 は `ato app session start` のみが対象。** その他の entry point は v1
以降で扱う。

| Entry point | v0 |
|---|---|
| `ato app session start` | **reuse enabled (本 RFC の対象)** |
| `ato run --background` | scope out (v1) |
| `ato run` (foreground) | scope out (v1) |
| `--reuse` flag | scope out (v1) |
| `--respawn` flag | scope out (v1) |
| `--require-session` flag | scope out (v1+) |

`ato app session start` から始める理由:
- PR 1 の baseline 計測がこの entry point。改善効果が直接測れる
- Desktop の orchestrator が呼ぶ唯一の経路で、UX への影響が最大
- `--json` envelope 契約が完成しており、reuse 時に追加の出力契約変更が不要
- detached session を返すだけで完結し、stdin / Ctrl+C / 終了コード等の
  ownership 問題が起きない

`ato run` foreground / background / 各 flag を後続にする理由:
- foreground は stdout/stderr stream / Ctrl+C / 終了コード契約が重く、
  既存 background process に attach する意味論が曖昧
- flag を増やすと CLI UX レビューが必要
- まずは reuse state machine の correctness を 1 経路で証明するのが筋

### 5.3 State Machine

v0 の action は **Reuse / Spawn の 2 値のみ**。既存 session を kill する
Replace は v1 に降格（§5.5）。

```
existing record found?
  no  → Spawn (cold)
  yes:
    schema_version >= 2?                    (record was written before v0)
      no  → Spawn (prior_kind=schema-too-old)
      yes:
        launch_digest match?
          no  → Spawn (prior_kind=digest-mismatch, leave old record alone)
          yes:
            pid alive (kill -0)?
              no  → Spawn (prior_kind=stale-session)
              yes:
                process_start_time_unix_ms == record.start_time?
                  no  → Spawn (prior_kind=pid-reuse-detected)
                  yes:
                    healthcheck succeeds (single attempt, timeout=1s)?
                      no  → Spawn (prior_kind=unhealthy-session)
                      yes → Reuse
```

すべての miss path は **既存プロセスを kill しない**。`launch_digest` が
変わっただけの古い session が別 UI（例: Desktop の別 tab、別 ato CLI 呼び出し）
に使われている可能性があるため、ownership / lifecycle の policy が固まる
までは spawn だけを行う。

`port-bound-by-pid` 検証は v1（§9.1）。strict port owner 確認は OS-specific
で実装重く、v0 の本筋から外れる。

Spawn 時、新しい session record（schema=2、新 PID / 新 port）を write する。
古い record はそのまま残す（multi-record state を許容、§8）。

### 5.4 Concurrent Start Locking

`acquire_lock(launch_key)` は次を保証する:

- 同じ `launch_key` を持つ 2 つの concurrent start は逐次化される
- 先行者が spawn 完了 + readiness wait 完了 + record write 完了するまで
  後続は進めない
- 後続が lock を取った時点で record を再 lookup → 先行者が書いた record を
  reuse することになる（race-free に reuse される）

**Lock の保持範囲は意図的に「lookup + spawn + readiness + record write」
全体である。** Readiness 待ちの前に lock を release してしまうと、後続の
caller は「ready な reusable record が無い」と観測して duplicate process を
spawn してしまう。Readiness wait まで lock を保持することで、後続は先行者の
session が ready になる瞬間まで待ち、その envelope を reuse できる
（duplicate spawn を回避できる）。

実装: `~/.ato/apps/<package_id>/sessions/locks/<launch_key>.lock` に
`fs2::FileExt::lock_exclusive`。lock file は持続させる（次回も同じファイルに
排他を取る）。lock 取得失敗時は max 60s 待ち、その後 timeout error。

別 capsule の起動はブロックしない（lock は per-launch_key なので独立）。

### 5.5 Stale record の扱い (v0) と Replace (v1+)

**v0 では既存 process を kill しない。** stale / digest mismatch /
unhealthy のいずれを観測しても、新しい session を spawn して新 record を
write するだけで、古い record と古い process は残置する。

理由:

- 古い session は別の UI（Desktop の別 tab、別 ato CLI 呼び出し）が依然
  使っている可能性がある。invisible に kill すると UX が壊れる
- 「誰が session を所有するか」(ownership) の policy が未定義のうちは
  destructive cleanup を避けるのが安全
- v0 のスコープ目的は「ready な session があるなら再利用する」ことであり、
  古い session を整理することではない

結果として v0 では同じ `launch_key` に対して **複数の record / 複数の
process が並存しうる**。lookup 時は launch_digest 一致 + 5 条件 (§9.1)
すべて pass するものを優先して reuse する。一致するものが無ければ
spawn。

**v1+** で以下を別 RFC として整理:

- ownership policy（どの caller が session を kill する権利を持つか）
- stale record の自動 GC（pid 死亡から N 時間で削除、TTL ベース、等）
- multi-instance を `--session=<name>` で明示識別
- log file rotation
- `Replace` action（kill + spawn）の destructive cleanup 契約

## 6. Phase Timing 表現

### 6.1 新 result_kind

v0 の result_kind は **2 値のみ**:

```
Existing (BUILD_MATERIALIZATION §6.1):
  executed
  not-applicable
  ...

Added by this RFC (v0):
  materialized-session         → reused, no spawn
```

v0 で `executed` を返す経路は次のいずれか:

- 既存 record なし（cold）
- record はあるが §5.3 のいずれかで pass しなかった（spawn 経路に倒れた）

reuse miss が起きた具体的な理由は **`prior_kind` extras** で診断する
（`result_kind` を増やさない）:

```
PHASE-TIMING phase=execute state=ok result_kind=executed elapsed_ms=883
            prior_kind="digest-mismatch" prior_pid="64200"
```

`prior_kind` 値集合 (v0):

| 値 | 意味 |
|---|---|
| `schema-too-old` | 既存 record が schema_version=1 |
| `digest-mismatch` | launch_digest が一致しない |
| `stale-session` | record の PID が死んでいる |
| `pid-reuse-detected` | PID alive だが process_start_time が一致しない |
| `unhealthy-session` | PID alive + start_time match だが healthcheck 失敗 |

v1 で追加予定: `replaced` (kill + spawn)、`missing-session`
(`--require-session` 付きで reuse 不可) — 本 RFC の対象外（§5.5、§8）。

### 6.2 Reuse path の sub-stages

PR 1 で追加した stage timer を流用:

```
# Reuse hit
PHASE-TIMING phase=execute stage=session_lookup state=ok elapsed_ms=2
PHASE-TIMING phase=execute stage=session_validate state=ok elapsed_ms=12
PHASE-TIMING phase=execute state=ok result_kind=materialized-session elapsed_ms=18

# Spawn (no record / cold)
PHASE-TIMING phase=execute stage=session_lookup state=ok elapsed_ms=2
PHASE-TIMING phase=execute stage=spawn_runtime_process state=ok elapsed_ms=1
PHASE-TIMING phase=execute stage=wait_http_ready state=ok elapsed_ms=876
PHASE-TIMING phase=execute stage=write_pid state=ok elapsed_ms=0
PHASE-TIMING phase=execute stage=write_session_record state=ok elapsed_ms=1
PHASE-TIMING phase=execute state=ok result_kind=executed elapsed_ms=883

# Spawn after reuse miss (digest mismatch / stale / unhealthy)
PHASE-TIMING phase=execute stage=session_lookup state=ok elapsed_ms=2
PHASE-TIMING phase=execute stage=session_validate state=ok elapsed_ms=8
PHASE-TIMING phase=execute stage=spawn_runtime_process state=ok elapsed_ms=1
PHASE-TIMING phase=execute stage=wait_http_ready state=ok elapsed_ms=874
PHASE-TIMING phase=execute state=ok result_kind=executed elapsed_ms=890
            prior_kind="digest-mismatch"
```

`session_validate` が schema check + launch_digest match + pid alive +
start_time match + healthcheck をカバーする（細分化は v1）。

## 7. CLI 互換性

### 7.1 新フラグ — v0 では追加しない

v0 では新 CLI flag を追加しない。`ato app session start` の挙動が透過的に
reuse-aware になるだけで、ユーザ向けの API 変更はない。

v1 で導入予定の flag（参考）:

| Flag | Entry points | 意味 |
|---|---|---|
| `--reuse` | `ato run` | foreground でも reuse を有効化（opt-in） |
| `--respawn` | `ato run`, `ato run --background`, `ato app session start` | 既存 session を kill して force spawn |
| `--require-session` | `ato run`, `ato app session start` | reuse できなければ fail with `ATO_ERR_MISSING_SESSION` |

### 7.2 既存挙動との互換

| 既存挙動 | v0 後 |
|---|---|
| `ato app session start <handle>` | **reuse 有効**。warm 2 回目以降 < 50ms。envelope 出力契約は不変 |
| `ato app session start <handle> --json` | 同上。reuse hit でも `SessionStartEnvelope` schema は同じ |
| `ato run .` (foreground) | 不変（毎回 spawn） |
| `ato run . --background` | 不変（毎回 spawn）。reuse 化は v1 |
| `ato run . --watch` | 不変。watch flow は本 RFC のスコープ外 |

`--watch` および dev profile（`next dev`）の整理は別 RFC（BUILD_MATERIALIZATION
§8 `Watch / Dev profile RFC` 参照）。

## 8. やらないこと（v0 スコープ外）

| 項目 | 先送り理由 |
|---|---|
| `ato run` foreground / background / `--reuse` / `--respawn` / `--require-session` | reuse state machine の正しさを `ato app session start` で先に証明する。foreground は stdout/Ctrl+C 契約が重く、別建てで議論したい。v1（§5.2、§7） |
| **Replace（既存 session の kill）** | ownership policy 未定義のうちは destructive cleanup を避ける。v0 では古い record を残置し、新 record を並存させる。v1+（§5.5） |
| **strict port-bound-by-pid 検証** | OS-specific（macOS `proc_pid_socket_info` / Linux `/proc/<pid>/net/tcp` / Windows `iphlpapi`）で実装重い。v0 では healthcheck で代替。v1（§9.1） |
| `prepare_session_execution` キャッシュ | PR 1 実測で 0ms。最適化対象外 |
| Multi-instance（同じ launch_key で複数 session 並走を identifier で区別） | 識別子（`--session=<name>`）の設計が必要。v1 |
| Stale record の自動 GC（pid 死亡から N 時間で削除、TTL ベース、等） | ownership policy と一緒に v1+ で扱う |
| Cross-host session reuse | portable session contract が未定義。L4 / L5（BUILD_MATERIALIZATION §8）と同じ整理 |
| Persistent daemon 化（Ato が長寿命プロセスとして capsule supervisor を兼ねる） | アーキテクチャ大改修。別 RFC |
| Desktop WebView preload / Surface materialization | Desktop 側の RFC で扱う |
| `npm:<package>` で pnpm shell shim を resolve する改修 | Node package bin 解決の独立 issue |
| Identity endpoint（spawned process が `ATO_SESSION_ID` / `ATO_LAUNCH_DIGEST` を expose） | アプリ側協力が必要。v1+ |

## 9. セキュリティ / 整合性

### 9.1 Reuse の必須検証セット（v0）

`pid alive (kill -0)` だけでは不十分。OS は親死亡後に PID を再利用するため、
他人のプロセスを「自分の session」と誤認するリスクがある。v0 では次の
**5 条件すべて**が pass で初めて Reuse 可（§5.3 の state machine と一致）:

1. session record の `schema_version >= 2`（v0 で書かれた record である）
2. `launch_digest` が現在の LaunchSpec と完全一致
3. `kill(pid, 0) == 0`（PID が alive）
4. `process_start_time_unix_ms` が record と完全一致（PID 再利用の検知）
   - macOS: `proc_pidinfo(PROC_PIDTBSDINFO).pbi_start_tvsec/usec`
   - Linux: `/proc/<pid>/stat` field 22 (`starttime` in jiffies) と
     `/proc/stat` の `btime` を合成して unix ms に変換
5. healthcheck が 1 回成功（§9.2 で定義した URL に対し、interval=25ms,
   timeout=1s の単発リクエスト）

5 条件のうち 1 つでも fail → Spawn（§5.5 のとおり、既存 process は kill
しない）。

**v1 で追加予定の条件**: port-bound-by-pid 検証（OS-specific な socket-to-PID
mapping）。v0 では healthcheck で port 上のサービスが少なくとも応答することを
代理確認する。port を別プロセスが奪っていても、healthcheck がその別
プロセスからは返ってこなければ unhealthy として Spawn 経路に倒れる。

### 9.2 Healthcheck URL の解決ルール

reuse 判定で叩く URL は session record の display category により決まる:

| Display category | URL の解決順 |
|---|---|
| `web` (runtime=web セッション) | `web.healthcheck_url` が record に書かれていればそれ。なければ `web.local_url` の host:port + `readiness.path`（v0 では `"/"`） |
| `guest` (guest WebView セッション) | `guest.healthcheck_url`（必ず record に書かれている） |
| `terminal` / `service` (非 HTTP) | reuse 判定対象外。常に Spawn（既存挙動と同じ） |

`http_get_ok` の既存 helper を再利用する（PR 1 で polling 25ms 化済み）。

### 9.3 ファイルシステム整合性

- session record の write は atomic temp+rename（既存 `write_session_record`
  の挙動を維持）
- lock file の lifecycle: 取得時に作成 / 残置（次回再利用）。stale lock は
  `fs2` の advisory lock が process 終了時に自動解放するため、明示削除不要

### 9.4 Secret / env 取り扱い

- `env_policy` は include set の **key 名** と **value の blake3 hash** のみ
  digest に取る（BUILD_MATERIALIZATION §4.3 と同じ）
- raw secret value は session record に保存しない
- spawn 時に inject される env は既存 `apply_allowlisted_env` の挙動を維持

## 10. 受け入れ条件 (Acceptance Criteria)

### 10.1 Baseline (PR 1 実測値)

```
samples/byok-ai-chat warm `ato app session start --json`:
  Total:                        885–1110 ms
  Build (materialized):         2–7 ms
  Execute total:                879–1099 ms
    └ wait_http_ready:          876–1095 ms
    └ everything else:          3–4 ms
```

### 10.2 v0 達成目標 (`ato app session start` のみ)

- [ ] 同 sample で warm 2 回目以降 (reuse-eligible session 存在時):
  - `result_kind=materialized-session`
  - Execute `elapsed_ms < 50ms`
  - new process が spawn されない（`pgrep -f next-server` の count が変わらない）
  - returned envelope の `pid` / `local_url` が既存 session を指す
- [ ] cold (record 不在) での挙動は現状維持（Execute ~880ms `result_kind=executed`）
- [ ] schema=1 の旧 record しか無い場合: spawn → schema=2 record を新規 write
  （`prior_kind=schema-too-old`）
- [ ] `ato run` foreground / `ato run --background` の挙動は不変（reuse 化は v1）

### 10.3 Stale 判定 — Spawn に倒れる経路

すべて **既存プロセスを kill せず、新 record を並存させて spawn する**こと
を確認する:

- [ ] PID 再利用シナリオ: record の PID を別プロセスが取得していた場合、
  `process_start_time` 不整合で Spawn（`prior_kind=pid-reuse-detected`）
- [ ] PID 死亡シナリオ: record の PID が無い場合、Spawn（`prior_kind=stale-session`）
- [ ] digest mismatch: capsule.toml の `run` を変更した場合、Spawn
  （`prior_kind=digest-mismatch`）
- [ ] Healthcheck 不通シナリオ: PID alive + start_time match + digest 一致
  だが `/` が 5xx / 接続不能を返す場合、Spawn（`prior_kind=unhealthy-session`）

### 10.4 並行 Start

- [ ] 同じ `launch_key` に対する 2 つの concurrent `ato app session start` で、
  spawn は 1 回のみ。後発は lock 取得後に先発の record を reuse（envelope の
  `pid` が一致する）
- [ ] 異なる `launch_key`（別 capsule、別 target）の concurrent start は
  互いをブロックしない
- [ ] lock は spawn + readiness wait + record write 全体で保持される
  （release タイミングが早すぎて duplicate spawn が起きない）

### 10.5 Phase Timing

- [ ] `ATO_PHASE_TIMING=1` で reuse hit 時に `session_lookup` / `session_validate`
  sub-stages が出る
- [ ] reuse hit で `result_kind=materialized-session` が PHASE-TIMING に出る
- [ ] reuse miss で spawn したとき、`prior_kind=...` extra が PHASE-TIMING に出る
  （該当する場合）

## 11. 移行パス

1. **v0 リリース直後**: 既存 capsule は `ato app session start` 経由で reuse
   の恩恵を自動的に受ける（schema=1 record は reuse 不可だが、次回 spawn で
   schema=2 で書き直される）。CLI flag は不変、Desktop の orchestrator も
   無改修で warm が短縮される。
2. **v1**: `ato run --reuse` / `--respawn` flag、`ato run --background` の
   reuse 化、`Replace` action（kill 既存 → spawn 新）、stale record の自動 GC、
   strict port-bound-by-pid 検証を追加。`[run.session].reuse` 宣言を
   canonical 化。
3. **v1.x**: multi-instance（`--session=<name>`）、`--require-session` 本実装、
   identity endpoint contract（`ATO_SESSION_ID` / `ATO_LAUNCH_DIGEST`）。
4. **v2**: cross-host portable session contract、daemon 化、Desktop Surface
   materialization との統合。

## 12. オープンクエスチョン

- **`process_start_time` 取得の Windows 対応**: macOS / Linux は §9.1 に書いた
  方法で取れる。Windows は `GetProcessTimes` で取得可能だが v0 ターゲットで
  ない。v0 では取得できなければ reuse 不可（spawn）に倒すことで安全側に
  寄せる
- **同 launch_key の record が複数並存したときの選択ロジック**: §5.5 で
  multi-record を許容したため、lookup で複数 hit する可能性がある。v0 では
  「`launch_digest` 一致 + 5 条件 pass のうち最も新しい `created_at`」を選ぶ
  ルールでよいか
- **`launch_digest` への OS arch 包含**: 現状の `toolchain_fingerprint` は
  OS/arch を含むため不要だが、明示が必要なら独立 field に
- **v1 で foreground reuse を入れるときの stdout/stderr semantics**: 「既存
  log file に tail する」「`--detach` で envelope を返して exit する」など
  複数案あり、v1 RFC で議論

## 13. 関連仕様 / 実装参照

- [BUILD_MATERIALIZATION.md](BUILD_MATERIALIZATION.md) — 同型の prior art。`prepare_decision` / `persist_after_execute` / `no_build_error` のパターンを本 RFC でも踏襲
- `apps/ato/crates/ato-cli/src/app_control/session.rs::start_runtime_session` / `start_guest_session` — Spawn 経路の実装。本 RFC v0 はこれの前段に reuse 判定を挿入
- `apps/ato/crates/ato-cli/src/app_control/session_runner.rs::SessionStartPhaseRunner` — Hourglass 経由の Execute phase 実装。reuse 経路は `run_execute` の冒頭に入る
- `apps/ato/crates/ato-cli/src/runtime/process.rs::ProcessManager` — pid 永続化と alive チェック。`process_start_time` 取得の実装場所候補
- `apps/ato/crates/ato-cli/src/application/pipeline/executor.rs::PhaseStageTimer` — sub-stage timing helper（PR 1 で追加）。reuse path の `session_lookup` / `session_validate` で再利用
- `fs2` crate — file-level advisory lock。§5.4 の `launch_key` lock で使用
