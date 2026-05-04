# 📄 Build Materialization Specification

**Document ID:** `BUILD_MATERIALIZATION`
**Status:** Draft v0.2
**Target:** ato-cli v0.5.x
**Last Updated:** 2026-04-28

## 1. 概要 (Overview)

`ato run` 実行時の build phase を「毎回実行する lifecycle step」から「declared
inputs から導出される materialized artifact」に再定義する。直接の動機は
`samples/byok-ai-chat` の warm 計測で build phase が 13–15 秒を占めており、
その間 ato 側に判定ロジックが一切存在しない（`preflight.rs:run_v03_lifecycle_steps`
は schema_version チェック以降は無条件で shell out）こと。

本仕様は短期的な build skip ではなく、Install / Prepare が既に CAS hit を
判定しているのと同じパターンを Build にも適用する**統一モデル**を定義する。

### 1.1 解決する問題

| 問題 | 具体 |
|---|---|
| 同一ソースに対する重複 build | `.next/` が存在するのに毎回 `next build` が走り、warm 13–15 秒 |
| 判定ロジックの不在（run path） | `run_v03_lifecycle_steps` は `provisioned_roots` HashSet 以外に skip 判定なし |
| Build が phase に固定されている | Install/Prepare 同等の materialization 抽象が無く、build だけが「実行 step」になっている |
| heuristic が contract に混入する危険 | Next.js / Cargo / uv ごとの個別最適化が積み上がると "Reuse the model, not special cases" に反する |

### 1.2 設計方針

- **Build は phase ではなく artifact**: `ato run` は declared inputs から
  導出される build artifact が local state 上で materialized 済みかを確認し、
  missing / stale のときだけ build executor を呼ぶ。
- **Declaration first, heuristic as migration aid**: `capsule.toml [build]` を
  正典とし、未宣言の場合のみ framework 別 heuristic を fallback として使う。
  heuristic 経由で動いた事実は recommendation として可視化する。
- **3 層モデルを汚さない**: 宣言（`capsule.toml`）／解決結果（`ato.lock.json`）／
  実機状態（`.ato/state/materializations.json`）を分離し、project 内に閉じる。
- **CAS / registry artifact は v0 の対象外**: portable artifact contract が
  未定義のため、cross-host / cross-worktree reuse は v1 以降。
- **Skip ではなく Materialized**: phase result は `state=skip` ではなく
  `state=ok result_kind=materialized` で表現する。既存の
  `HourglassPhaseResult.result_kind` をそのまま流用。
- **Schema は bump しない**: v0 は `schema_version = "0.3"` の forward-compatible
  extension として `[build]` を許容する（§3.0）。

### 1.3 既存 `ato build` 側 build cache との関係

現行 repo には既に `ato build` 側に v0.3 build cache 実装が存在する
（`apps/ato/crates/ato-cli/src/cli/commands/build.rs:1097` 周辺）。

| 項目 | 既存 `ato build` cache | 本 RFC v0 (`ato run` materialization) |
|---|---|---|
| 対象コマンド | `ato build` | `ato run` の build phase |
| 入口 | `prepare_v03_build_cache()` | `run_v03_lifecycle_steps` 直前 |
| Cache 種別 | output tree を copy/restore する artifact cache | 既に worktree に存在する output を再利用する materialization record |
| Storage | `<nacelle_home>/build-cache/chml/<sha256-key>` | `<workspace>/.ato/state/materializations.json` |
| Digest | SHA-256（`BUILD_CACHE_LAYOUT_VERSION = "chml-build-cache-v1"`） | blake3（§9.1） |
| Output 検証 | restore 時に tree 全体を copy back | existence check のみ（§9.2） |
| Scope | global（host 全体で共有） | project worktree 内に閉じる |

**v0 の方針**:

- 既存 `ato build` cache は**置き換えない**。ato build は引き続き copy/restore 型 cache を使う。
- 共通する **path normalization / safety check / source walker** の helper
  （`normalize_build_cache_outputs`, `BUILD_CACHE_IGNORED_DIRS`, `collect_build_cache_source_files`,
  `native_lockfiles_for_build_cache`）は **shared module に extract して両者で reuse する**。
- digest 算法は意図的に分ける（blake3 vs SHA-256）。理由は §9.1。
- v1 で両者の digest 抽象を統合し、output tree を CAS-of-output に乗せる
  ときに、`ato build` の copy/restore cache と本 RFC の materialization record
  を一本化することを検討する。

## 2. コアコンセプト (Core Concepts)

### 2.1 Build Artifact

Build phase は次の関数として再定義される。

```
build_artifact = build_executor(inputs, command, scope, toolchain_env)
```

- `inputs`: declared input file set（glob で展開される）
- `command`: build を実行する shell command
- `scope`: selected target label と working directory（multi-target capsule で
  build artifact を分離する key の一部、§4.1）
- `toolchain_env`: Node / package manager / OS の version など、build 出力に影響する環境
- 出力: declared output paths（dir / file）

### 2.2 Input Digest

`inputs`, `command`, `scope`, `toolchain_env`, declared `env.include` から
決定論的に算出される identifier。

```
input_digest = blake3(
  schema_version_marker     ||  // "ato-build-materialization-v1"
  selected_target_label     ||
  working_dir_relative      ||
  command                   ||
  normalized_outputs        ||
  build_spec_source         ||  // "declared" | "heuristic:<name>:<version>"
  toolchain_fingerprint     ||
  canonical_env             ||  // sorted (key, blake3(value)) for env.include
  canonical_inputs          ||  // sorted (relative_path, blake3(file_bytes))
)
```

`canonical_inputs` は path を sort し、各 file 本体を blake3 で stream hash した
`(path, hash)` ペアの再 hash である。

### 2.3 Materialization Record

直前の build 成功を記録する project-local state（実機状態レイヤ）。

```
<workspace>/.ato/state/materializations.json
```

`ato.lock.json` には入れない（lock は "解決結果" 層であり、host-local な
artifact 状態は持ち込まない、§7）。

## 3. スキーマ定義 (`capsule.toml`)

### 3.0 Schema バージョン方針

**実装上の制約 (v0 actual):** capsule-core の既存 `BuildConfig` schema は
v0.3 manifest で `[build]` を予約済みであり、`[build].lifecycle.build`,
`[build].inputs` (`{ lockfiles, toolchain, ... }` の typed table),
`[build].outputs` (`{ capsule, sha256, ... }`) という別構造を持つ。本 RFC の
`[build].command` / `[build].inputs = [...]` (glob リスト) /
`[build].outputs = [...]` は既存 schema と TOML レベルで衝突するため、
**v0 では declared `[build]` 宣言を実装スコープから外す**。

v0 の materialization は **framework heuristic 経由のみ**で動作する
（§3.3）。declared spec を canonical にするには manifest schema を 0.4 に
bump し、capsule-core BuildConfig を再設計する別 RFC が必要。これは v1 で
扱う。

v0 の TOML では `build = "npm run build"` の従来形式を維持する。
materialization は plan の `build_lifecycle_build()` をそのまま command
ソースとして読み、heuristic で inputs/outputs を推論する。

### 3.1 `[build]` セクション (v1+ で導入予定 — v0 では非対応)

> **v0 では受け付けない**。capsule-core の既存 `BuildConfig` schema と TOML
> レベルで衝突するため、parser は `[build]` table を読まない。下記は v1 で
> 目指す形を残しておくための参考スキーマ。

```toml
[build]
command = "npm run build"
inputs = [
  "package.json",
  "pnpm-lock.yaml",
  "next.config.*",
  "tsconfig.json",
  "src/**",
  "app/**",
  "public/**",
]
outputs = [".next"]

[build.env]
include = ["NODE_ENV", "NEXT_PUBLIC_*"]
```

| Field | Type | Required | 意味 |
|---|---|---|---|
| `command` | string | yes | build を実行する shell command。`build_lifecycle_build()` と等価 |
| `inputs` | string[] | yes | input file glob のリスト。順序は digest に影響しない |
| `outputs` | string[] | yes | build 出力 path のリスト。skip 判定で「全て存在するか」を確認 |
| `env.include` | string[] | no | input digest に取り込む env 変数名（glob 可）。デフォルトは空 |

### 3.2 Path policy（inputs / outputs 共通）

下記は v0 仕様（open question ではない）。

- **Relative paths only**: working_dir を起点とする相対パス。absolute path
  および parent traversal (`..`) は parser で reject する。
- **Glob metacharacters**: `outputs` は `<dir>/**` を末尾の dir-tree 表現として
  normalize する。それ以外の `*` `?` `[` は reject。`inputs` は `**` および
  `*` `?` `[` を glob として展開する（既存 `ato build` cache と同じ）。
- **Symlinks**: 追跡しない（symlink metadata と link target を hash する）。
  working_dir を抜ける symlink は digest 計算で reject。
- **Hidden files**: glob が明示的にマッチした場合のみ含める（`**/*` は dotfile
  を含まない既定）。
- **Unreadable / broken symlink**: digest 計算 fail（材料化判定はできない）。
  build はそのまま走る。
- **既存 helper を reuse**: `normalize_build_cache_outputs()` の安全性チェックと
  `BUILD_CACHE_IGNORED_DIRS` (`.git`, `.tmp`, `node_modules`, `.venv`, `target`,
  `__pycache__`) の除外、および `collect_build_cache_source_files()` の walker
  をそのまま使う（§1.3 参照）。

### 3.3 Heuristic Fallback

`[build]` が未宣言かつ `build_lifecycle_build()` が存在する場合のみ heuristic を
適用する。**heuristic は contract ではなく migration aid である**。

heuristic は **inclusive list 型ではなく exclusive list 型**を採用する。
理由: Next.js では `components/`, `lib/`, `styles/`, `middleware.ts`,
`postcss.config.*`, `tailwind.config.*`, monorepo workspace pkg 等 build に
影響する dir/file が多岐にわたり、列挙では false materialization を起こしやすい。

#### v0 で対応する framework

| Framework | 検出条件 | 戦略 |
|---|---|---|
| Next.js | `next.config.*` 存在 OR `package.json` `dependencies.next` 存在 | exclusion-based（下記） |
| Vite | `vite.config.*` 存在 | exclusion-based（下記） |

Cargo / Python uv は v0 では heuristic 対象外（uv の `.venv` は build artifact
ではなく provision artifact、Cargo は bin 名推定が複雑なため）。

#### Exclusion-based inputs

```text
inputs:
  "**/*"

exclude:
  既存 BUILD_CACHE_IGNORED_DIRS: .git, .tmp, node_modules, .venv, target, __pycache__
  + framework outputs (e.g. .next, dist)
  + ato local state: .ato
  + 一般的な cache: .turbo, .vercel, coverage
```

#### Heuristic outputs

| Framework | outputs |
|---|---|
| Next.js | `.next` |
| Vite | `dist` |

#### Heuristic Versioning

heuristic 定義の変更は input_digest を変えるべきなので、`build_spec_source`
には heuristic 名と version を含める。

```
build_spec_source = "heuristic:nextjs:v1"
```

heuristic 定義を変えたら `v1` → `v2` に bump する（自動的に既存 record が
stale になり再 build される）。

### 3.4 Recommendation Output

heuristic fallback が使われた最初の materialization 時に、stderr へ次を
1 度出力する（同一 input_digest に対する 2 回目以降は出さない）。

```
ATO-RECOMMEND build inputs were inferred for "nextjs" framework.
              Declare [build] inputs/outputs in capsule.toml for stable
              materialization. See: docs/rfcs/draft/BUILD_MATERIALIZATION.md
```

| Mode | Recommendation 出力 |
|---|---|
| Normal TTY | 出す |
| `--json` | 出さない |
| `--quiet`（将来導入時） | 出さない |
| 同一 digest で record 既存 | 出さない |
| `ATO_PHASE_TIMING=1` | recommendation の有無に独立。phase timing は出す |

「最初の 1 度」の判定は state 上の `recommendations_emitted` 配列で記録する
（§4.1）。

## 4. State Schema (`.ato/state/materializations.json`)

### 4.1 Record 構造

multi-target / multi-working_dir に対応するため、record key は
`(target, working_dir, name)` の組で一意化する。

```json
{
  "schema_version": 1,
  "artifacts": [
    {
      "name": "build",
      "target": "app",
      "working_dir": ".",
      "input_digest": "blake3:abcdef...",
      "command": "npm run build",
      "outputs": [".next"],
      "source": "heuristic",
      "heuristic": "nextjs:v1",
      "toolchain_fingerprint": "node:v20.11.0|pnpm:10.15.0|darwin-arm64|chml-mat-v1",
      "env_fingerprint": "blake3:...",
      "env_keys": ["NODE_ENV", "NEXT_PUBLIC_API_BASE"],
      "created_at": "2026-04-28T13:21:55Z"
    }
  ],
  "recommendations_emitted": [
    {
      "kind": "declare-build",
      "input_digest": "blake3:abcdef...",
      "heuristic": "nextjs:v1"
    }
  ]
}
```

| Field | 必須 | 意味 |
|---|---|---|
| `schema_version` | yes | このファイルの schema 番号 |
| `artifacts[].name` | yes | artifact 種別。v0 では `"build"` のみ |
| `artifacts[].target` | yes | selected target label |
| `artifacts[].working_dir` | yes | workspace root からの相対 path |
| `artifacts[].input_digest` | yes | §2.2 で算出した digest |
| `artifacts[].command` | yes | 実行された build command（diagnostic 用） |
| `artifacts[].outputs` | yes | 期待される出力 path |
| `artifacts[].source` | yes | `"declared"` または `"heuristic"` |
| `artifacts[].heuristic` | source=heuristic 時 | `<name>:<version>` |
| `artifacts[].toolchain_fingerprint` | yes | host 環境の identifier（§4.2） |
| `artifacts[].env_fingerprint` | yes | env.include 値の hash（key と value） |
| `artifacts[].env_keys` | yes | digest に含めた env 変数の key list（diagnostic 用） |
| `artifacts[].created_at` | yes | RFC 3339 timestamp |
| `recommendations_emitted` | yes | 推奨表示済みリスト |

**state record に `result_kind` は持たない**。`result_kind` は phase result の
概念であり、state record は「成功した materialization」自体を表す（常に
materialized 状態）。

### 4.2 Toolchain Fingerprint

```
node:v20.11.0|pnpm:10.15.0|darwin-arm64|chml-mat-v1
```

取得優先順位:

1. `RuntimeLaunchContext` / resolved runtime info から取れる値を使う
2. それで埋まらない要素は `"unknown"` を入れる
3. `"unknown"` も digest に含める（次回 resolve 結果が変わると再 build）

含める要素:

- OS / arch（`darwin-arm64` など）
- runtime kind / driver（`web/node` / `cargo` 等）
- runtime version（node, pnpm, etc.）
- selected target label
- working_dir relative path
- materialization schema version (`chml-mat-v1`)
- heuristic version（heuristic fallback 時のみ）

### 4.3 Env Security Policy

- `env.include` は **key と value の blake3 hash** を digest に含める
- raw value は `materializations.json` に保存しない（secret 漏洩防止）
- `env_keys` には key 名のみ列挙
- `env_fingerprint` は sorted (key, blake3(value)) の再 hash
- missing env と empty env は区別する（前者は `"<missing>"` をマーク、`build.rs:1209` と同じ）
- recommendation / debug log に raw env value を出さない

### 4.4 Atomicity / Concurrency

- write 時は `materializations.json.tmp` に書いて rename
- 書き込みは file lock で排他（既存 `capsule_core` の `persist_noclobber` 相当）
- parse 失敗時は warn を出し record 不在として扱う（再 build に倒れる）

## 5. 実行フロー

### 5.1 Build Phase

```text
build phase entry
├─ resolve build command（§5.4 の解決順）
├─ if no build command available:
│    → result_kind=not-applicable, return
├─ resolve build spec:
│    [build] declared → source=declared
│    else heuristic match → source=heuristic
│    else → result_kind=not-applicable, return
├─ compute input_digest (§2.2)
├─ load .ato/state/materializations.json (may be absent or invalid)
├─ policy 判定（§5.2）:
│    rebuild  → execute build
│    no-build → record の状態に応じて細分化された fail（§5.3）
│    if-stale (default):
│      record (target, working_dir, name) lookup
│        miss          → execute build
│        digest mismatch → execute build
│        outputs missing → execute build
│        all match     → materialized（shell out しない）
└─ on execute:
     run build_lifecycle_build（既存パスを reuse）
     update materializations.json
     emit recommendation if heuristic & first occurrence
```

### 5.2 Build Policy

| Policy | CLI flag | 意味 |
|---|---|---|
| `if-stale` | (default) | record と一致かつ outputs 存在のときのみ skip |
| `always` | `--rebuild` | 強制 build。record 上書き |
| `never` | `--no-build` | build 禁止。失敗条件は §5.3 |

policy は CLI flag のみ。`capsule.toml` には書かない。

### 5.3 `--no-build` Failure Modes

`--no-build` で fail する場合、phase result の `result_kind` を細分化する
（診断のため）。エラーコードは v0 では 1 種に統一。

| 状態 | result_kind | error code |
|---|---|---|
| record なし | `missing-materialization` | `ATO_ERR_MISSING_MATERIALIZATION` (E5xx) |
| digest mismatch | `stale-materialization` | 同上 |
| outputs 不在 | `missing-outputs` | 同上 |
| state file 破損 | `invalid-materialization-state` | 同上 |
| build spec 解決失敗 | `unresolved-build-spec` | 同上 |

### 5.4 Build Command Resolution Order

`[build].command` は canonical declaration である。v0 実装は既存
`build_lifecycle_build()` を compatibility source として使う。

```text
1. [build].command          (declared, canonical)
2. build_lifecycle_build()  (legacy compatibility)
   = targets.<target>.build_command
   ∪ build.lifecycle.build
3. heuristic outputs only（command なし → not-applicable）
```

将来 (v0.next 以降) `build_lifecycle_build()` を `[build].command` の compat
adapter に統合する予定だが、v0 は両者を併存させる。

### 5.5 Provision との分離

V0.3 lifecycle の `provision`（`pnpm install` など）は input digest 判定対象から
**除外**する。理由:

- provision は `node_modules/` という巨大かつ volatile な状態を生む
- pnpm / npm 自体が lockfile ベースの skip 判定を持つ（二重判定は不要）
- byok-ai-chat 計測では warm provision (`pnpm install`) は約 400ms。
  Install/Prepare hourglass phase は 1ms 程度。本 RFC が削減対象とする
  build phase 13–15s に比べ十分小さい

将来 `[provision]` を別 artifact として扱うのは v1 以降。

## 6. Phase Timing 表現

`HourglassPhaseResult.result_kind` を流用する。`PHASE-TIMING` の出力例:

```text
# materialization hit (declared)
PHASE-TIMING phase=build state=ok result_kind=materialized source=declared elapsed_ms=2

# materialization hit (heuristic)
PHASE-TIMING phase=build state=ok result_kind=materialized source=heuristic heuristic=nextjs:v1 elapsed_ms=2

# 実行された (declared)
PHASE-TIMING phase=build state=ok result_kind=executed source=declared elapsed_ms=13056

# 実行された (heuristic)
PHASE-TIMING phase=build state=ok result_kind=executed source=heuristic heuristic=nextjs:v1 elapsed_ms=13056

# build command そのものが存在しない
PHASE-TIMING phase=build state=ok result_kind=not-applicable elapsed_ms=0

# --no-build で record が存在しない
PHASE-TIMING phase=build state=fail result_kind=missing-materialization elapsed_ms=1

# --no-build で digest mismatch
PHASE-TIMING phase=build state=fail result_kind=stale-materialization elapsed_ms=2

# --no-build で outputs missing
PHASE-TIMING phase=build state=fail result_kind=missing-outputs elapsed_ms=2

# state file 破損
PHASE-TIMING phase=build state=fail result_kind=invalid-materialization-state elapsed_ms=1
```

### 6.1 phase result `result_kind` 値集合（v0）

```
executed
materialized
not-applicable
missing-materialization
stale-materialization
missing-outputs
invalid-materialization-state
unresolved-build-spec
```

`state=skip` は build phase からは出さない（"skip" は "phase 自体が selection
外" の意味に限定）。

## 7. Lock Layer 統合の延期

v0 は `ato.lock.json` を**触らない**。理由:

- lock は declaration の解決結果を記録する層であり、host-local な materialization
  状態を入れるべきではない
- canonical build spec の lock 書き込みは別 RFC（lock layer 改修）が必要

> Lock-layer integration is intentionally deferred. v0 computes the resolved
> build spec at runtime and stores only host-local materialization state.
> A future lock-layer RFC may persist canonical build specs or locked artifact
> digests.

## 8. CLI 互換性

| 既存挙動 | v0 後 |
|---|---|
| `ato run .` | input digest 一致なら build 自動 skip |
| `ato run . --watch` | watch flow は本 RFC のスコープ外（別 RFC） |
| `ato run . --background` | 同上、build phase の挙動は本 RFC に従う |

新 flag:

- `--rebuild`: 強制 build。materialization 記録を上書き
- `--no-build`: build 禁止

`schema_version = "0.3"` のままで `[build]` を許容するため、既存サンプルは
無改修で速度改善を受ける（§3.0）。

## 9. セキュリティ / 整合性

### 9.1 Digest 算法

- 本 RFC は **blake3** を採用する（既存 CAS と整合し、高速）
- 既存 `ato build` の v0.3 cache は **SHA-256** (`BUILD_CACHE_LAYOUT_VERSION =
  "chml-build-cache-v1"`) を使い続ける
- 両者は意図的に**分離**する。混同を避けるため、新 digest は `blake3:` プレフィクスを必ず付ける
- v1 で digest abstraction を切り、両者を統合することを将来 work として明記

### 9.2 Outputs 検証粒度（v0）

> v0 は outputs の **存在のみ**を確認する。outputs 内部の破損や部分的な stale
> は検知しない。`.next/` の `BUILD_ID` だけが消えた、`.next/server/` の中身が
> 部分的に壊れた、といった状況は materialized 扱いになり再 build されない。
> 破損時は `--rebuild` で回復する。output tree hash validation は v1 の範囲とする。

存在チェックの粒度:

- declared output が dir なら、dir 自体が存在し空でないことを確認
- declared output が file なら、file が存在しサイズ > 0 を確認

これ以上の検証（sentinel file, framework-specific marker）は heuristic を増やす
ため v0 では入れない。

### 9.3 入力の安全性

- §3.2 path policy 参照
- 入力に command 文字列が含まれるため、`build` command を変更したら必ず再 build される
- env value は raw を保存しない（§4.3）

### 9.4 State 破損時の挙動

- parse 失敗 → warn を stderr に出し、record 不在として扱う（自然に build が走る）
- write 失敗 → stderr に warn、build 自体の成否は影響しない

## 10. 受け入れ条件 (Acceptance Criteria)

### 10.1 byok-ai-chat 実測

- [ ] warm 2 回目以降で build phase `elapsed_ms < 100`、`result_kind=materialized`
- [ ] `--rebuild` 付きで build phase `elapsed_ms ≈ 13–15s`、`result_kind=executed`
- [ ] `--no-build` かつ materialization 不在で fail（`result_kind=missing-materialization`）

### 10.2 Materialization の妥当性

- [ ] `[build]` 宣言ありの sample でも heuristic と同等に動作（`source=declared`、recommendation 抑止）
- [ ] `inputs` のいずれかを 1 byte 変更すると input_digest が変化し再 build
- [ ] `command` を変更すると input_digest が変化し再 build
- [ ] `env.include` 対象 env の値を変更すると input_digest が変化し再 build
- [ ] selected target が異なる場合に別 record になる（同一 capsule の multi-target）
- [ ] working_dir が異なる場合に別 record になる
- [ ] heuristic から declared に capsule.toml を変更すると `source` が `declared` に切り替わる
- [ ] `outputs` directory を rm すると次回 build が走る（existence check）
- [ ] `outputs` 内部 file を 1 つ削除しただけでは v0 では検知しない（§9.2、明示的に non-goal として test）

### 10.3 Path Policy

- [ ] `outputs = ["../foo"]` parse 時に reject
- [ ] `outputs = ["/abs/path"]` parse 時に reject
- [ ] `outputs = ["a/*"]` parse 時に reject（dir/** 以外の glob）
- [ ] `outputs = ["a/**"]` は `a` に正規化される
- [ ] `inputs = ["../bar/**"]` parse 時に reject
- [ ] working_dir を抜ける symlink を入力に含むと digest 計算 fail

### 10.4 State の堅牢性

- [ ] `materializations.json` を削除すると次回 build が走る
- [ ] `materializations.json` の JSON が壊れていると warn + 再 build（fail せず）
- [ ] 同時実行（同一 workspace に 2 プロセス）で write レースが起きない（file lock）

### 10.5 Phase Timing

- [ ] `ATO_PHASE_TIMING=1` で result_kind が出力される
- [ ] `ATO_PHASE_TIMING=1` で source / heuristic が出力される
- [ ] `--rebuild` で `result_kind=executed`
- [ ] `--no-build` で `result_kind=missing-materialization|stale-materialization|missing-outputs` のいずれか

### 10.6 Recommendation

- [ ] heuristic で 1 回目に `ATO-RECOMMEND` が stderr に出る
- [ ] 同一 digest の 2 回目には出ない
- [ ] `--json` 時に出ない
- [ ] declared 時には出ない

## 11. 移行パス

1. **v0 リリース直後**: 既存 capsule は heuristic fallback で動く。recommendation のみ出る。
2. **v0.x の中で**: 主要サンプル（byok-ai-chat 含む）に `[build]` を宣言し、heuristic から declared に移行。
3. **v0.next**: `[build]` 未宣言かつ build command ありのケースで warning レベルに引き上げ（fail はしない）。
4. **v1**: schema_version 0.4 で `[build]` 必須化を検討。同時に L4 (CAS-of-output) / L5 (registry artifact) の RFC を起こし、既存 `ato build` cache との統合も行う。

## 12. オープンクエスチョン

- multi-target capsule で複数 `[build]` をどう宣言するか（v0 は `[build]` 単一、target 違いは current selected target で digest 分離）。`[build.<target>]` 化は v1
- `outputs` の path に dir / file の混在があるときの存在判定の表現（v0 は metadata で type 判定）
- `RuntimeLaunchContext` から runtime version を引けない経路（compat fallback など）の `"unknown"` 比率が高い場合の運用

## 13. 関連仕様 / 実装参照

- [LIFECYCLE_SPEC.md](../accepted/LIFECYCLE_SPEC.md) — Task/Service lifecycle。本 RFC は build を artifact に再定義するため、lifecycle の `setup` とは別軸。
- [PURE_TRANSFORMS_AND_LOCK_LAYERS.md](PURE_TRANSFORMS_AND_LOCK_LAYERS.md) — Pure transform / lock layer モデル。本 RFC の v1 (CAS-of-output) はここに接続する。
- `apps/ato/crates/ato-cli/src/cli/commands/run/preflight.rs` — `run_v03_lifecycle_steps` の現行実装。本 RFC v0 はここに判定を挿入する。
- `apps/ato/crates/ato-cli/src/cli/commands/build.rs` — 既存 v0.3 build cache。`prepare_v03_build_cache` (line 1097), `normalize_build_cache_outputs` (1132), `compute_v03_build_cache_key` (1179), `BUILD_CACHE_IGNORED_DIRS` (26), `collect_build_cache_source_files` (1245) を helper として共有 module に extract して reuse する。
- `apps/ato/crates/ato-cli/src/application/pipeline/hourglass.rs` — `HourglassPhaseResult.result_kind` を流用する。
- `apps/ato/crates/ato-cli/src/application/pipeline/executor.rs` — `ATO_PHASE_TIMING` 計測基盤（PR-A で `result_kind` を含む形に拡張）。
