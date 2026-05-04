---
title: "Capsule Dependency Contracts"
status: accepted
accepted_date: "2026-05-04"
draft_history: "v1.0 → v1.5 (4 review rounds)"
date: "2026-05-04"
revision: "v1.5"
author: "@koh0920"
related:
  - "docs/rfcs/draft/CAPSULE_URL_SPEC.md"
  - "docs/rfcs/draft/HASH_AND_PROVENANCE_POLICY.md"
  - "docs/rfcs/draft/DEPENDENCY_DERIVATION_CACHE.md"
  - "docs/rfcs/draft/beyond-reproducible-build.md"
  - "docs/rfcs/draft/ATO_HOME_LAYOUT.md"
  - "docs/rfcs/accepted/CAPSULE_HANDLE_SPEC.md"
  - "docs/rfcs/accepted/IDENTITY_SPEC.md"
---

# Capsule Dependency Contracts

> Status: **Accepted (v1.5)** — 2026-05-04. v1 scope は §3、follow-up は §12。実装計画は本ファイル末尾の §15 / `docs/plan_capsule_dependency_contracts_20260504.md` 参照。
>
> Ato が依存先 capsule (service / tool / app) を **同一の識別子空間と同一の lock セマンティクス** で扱うための grammar を定義する。`managed_services` のような実装由来の語彙は外部仕様に出さず、`capsule dependency graph + service contract + exports + needs + state binding` のみを公開語彙とする。
>
> v1.5 は v1.4 のレビュー (Philosophy Compliance Review #4 2026-05-04) を反映し、(a) Non-Goals (§2) の stale な「secret は通常 parameter として流す」記述を credentials block 体制へ整合、(b) reserved variant の Non-Goals 記述を「parse OK / lock fail-closed」表現に統一、(c) `credentials.<key>.default` を v1 で **禁止** (Safe by default の強化)、(d) credential メモリ/露出ガイドを「**必須保証**」と「**努力義務**」に分離 (§7.3.2 Rule M4 を再構成)、の 4 点で残った stale 文言と緩い境界を解消した。

## 1. Motivation

現状の Ato では「capsule が外部サービスに依存する」ことを宣言する一級市民の語彙が無い。WasedaP2P backend (`capsule://github.com/Koh0920/WasedaP2P`) は `capsule.toml` のコメントで以下のように外部前提を要求している:

```text
Prerequisite NOT shipped with this capsule:
    PostgreSQL 14+ reachable at DATABASE_URL.
```

実行時には FastAPI の lifespan で `127.0.0.1:5432` を引きに行き、Postgres 不在時に `psycopg.OperationalError: Connection refused` で exit 3 する (2026-05-04 観測の WasedaP2P session log を参照)。これは:

1. capsule manifest が「自分が必要とする runtime 依存」を機械可読に宣言できていない
2. Ato 側にそれを起動する schedule / lifecycle が無い
3. 依存先 (Postgres) が Ato 哲学から外れた特殊なビルトイン (e.g. `kind = "postgres"`) として扱われると、新しい service kind ごとに Ato 本体に変更が要る

の三層の問題に分解できる。本 RFC は (1)(2)(3) を以下の単一の枠組みで解決する:

> **依存先は Postgres という固有名詞ではなく、`capsule://` で識別される普通の capsule であり、`service contract` を実装することで起動順序・healthcheck・exports の語彙が成立する。**

このモデルでは、Postgres は Ato 本体の特殊機能ではなく **service contract を実装する 1 個の capsule** にすぎない。新しい service kind を増やしても Ato コアは変わらない。

## 2. Non-Goals (v1)

以下は v1 から **意図的に除外** し、follow-up RFC で扱う。早期に複雑度を持ち込まないため:

- `contract = "tool@1"` の実行セマンティクス (invoke convention, args/stdin/stdout)。v1 は `service@1` のみ。
- `state.ownership = "shared"` の実装。v1 は `parent` のみ。`shared` は予約語として parser で reject。
- 同名 capsule の major-version 衝突解決 (npm 風 nested vs flat)。v1 は「同 graph 内で同 capsule の異 major は禁止」とする。
- 同一 service instance を複数 consumer で共有する仕組み (refcount)。v1 は **1 consumer per instance**。
- transitive dependency の v2 receipt への取り込み (`dependency_derivation_hash` の再帰拡張)。Phase 2 cross-host reconstruct と一括で扱う。
- 依存先 capsule の signature / supply chain 検証ポリシー。`HASH_AND_PROVENANCE_POLICY.md` の domain 区切りに従い、別 RFC で。
- **`parameters.<key>.secret = true` 構文 (provider/consumer 双方)**。secret 値は v1 では `[dependencies.*.credentials]` / `[contracts.*.credentials]` block でのみ宣言・伝達し、identity / state path に絶対に入れない (§7.3.1)。secret を identity に取り込む hash-of-value 方式は follow-up RFC `CAPSULE_DEPENDENCY_SECRET_IDENTITY` で再検討。
- **`credentials.<key>.default` の literal 値**。v1 では default の宣言を **禁止** (§7.3.1)。provider manifest に「既定パスワード」「既定 token」を埋め込む誘惑を構文時点で排除。`required = false` で「未指定なら credential なし」のみ許す。default を提供したい場合は follow-up `CAPSULE_DEPENDENCY_CREDENTIAL_DEFAULTS` で扱う。
- **`unix_socket = "auto"` の dynamic endpoint allocation**。v1 では parser は AST に受理するが **lock verification で fail-closed** (§9.1 verification 13)。v1 主経路は `port = "auto"` (TCP) のみ。
- **`ready` の `http` / `unix_socket` variant**。v1 は `tcp` と `probe` のみ。`http` / `unix_socket` は parser は AST に受理するが **lock verification で fail-closed** (§9.1 verification 13)。
- **orphan auto-action** (kill / GC)。v1 は detection + warn のみ。data dir 競合の安全性は provider 側の data-dir locking (Postgres `postmaster.pid` 等) に依拠。
- **credential 値の zeroize による hard guarantee**。v1 では努力義務 (§7.3.2 Rule M4)。Rust `zeroize` 等の memory clearing を v1.x で hard guarantee に格上げ。なお credential 値を lock / receipt / logs / argv / shell command body に残さないことは v1 でも **必須保証** で、努力義務ではない。
- observability (dependency lifecycle の trace / receipt 表示 / `ato explain-hash` の dep 説明)。v2 RFC で。

## 3. v1 Scope

本 RFC v1 で固定する 11 項目:

1. `capsule://` dependency identifier grammar (§4)
2. `[dependencies.*]` block grammar (§5)
3. `[dependencies.*.contract]` (§6)
4. provider 側 `[contracts.<name>@<major>]` block (§7)
5. **service contract → target binding** (§7.2)。`service@1` の本質は **ready + exports contract** であり endpoint allocation は手段にすぎない。target に endpoint 宣言があるかは contract 必須要件ではない。
6. **parameters と credentials の分離** (§7.3 / §7.3.1)。`[contracts.*.parameters]` は identity-bearing (instance_hash と `dependency_derivation_hash` に入る)、`[contracts.*.credentials]` は runtime-only で **identity に絶対に入らない**。secret/credential rotation が state path を変えないことを構文で保証する。
7. **Credential Materialization Rules M1–M5** (§7.3.2)。plaintext textual substitution を v1 で禁止し、stdin / temp file / env channel のいずれかで provider process に届ける。argv / shell command body / log への露出を防ぐ。
8. `identity_exports` / `runtime_exports` の分離 (§7.4)、env capture model に対する **origin tracking 要求** (§7.4.1)、env 注入の主経路は **`runtime_exports`**
9. `port = "auto"` の TCP dynamic endpoint allocation (§7.5)。`unix_socket = "auto"` は **parse OK / lock fail-closed** (§9.1)。
10. **state path rule** with provider-declared `state.version`、parameters のみから導出する **instance hash** (§7.7)。alias / credentials は path にも identity にも入らない。
11. **dep env resolution scope** (§5.2)。`{{env.X}}` in `[dependencies.*]` は manifest top-level `required_env` のみから解決される。target-local `required_env` は対象外。

加えて以下の semantic を §8〜§10 で確定する (構文ではなく invariant):

- lifecycle uniformity invariant (§5 末尾)
- `dependencies` ⊇ `needs` integrity (§8)
- lock-time vs runtime-time verification の責務分離 (§9)、reserved variant は **fail-closed**
- alias / credentials / `runtime_exports` は instance_hash・identity に入らない (§9.3)
- teardown ordering (§10.4)。v1 は orphan は **warn-only**、auto-kill / auto-GC は follow-up
- `runtime_exports.<key>.secret = true` は **redaction-only** (output 表示用途)。identity には影響しない

## 4. `capsule://` Dependency Identifier

`CAPSULE_URL_SPEC.md` の URL grammar に準拠する。本 RFC では依存宣言で受け入れる **authority + ref** の意味付けのみ追加で固定する。

### 4.1 受け入れる形

```text
capsule://<authority>/<path>@<ref>
```

- `authority` の予約語 `ato` (= `capsule://ato/...`) は **official Ato registry** とみなす。`ato` は DNS hostname ではない reserved keyword。
- `<ref>` は (a) 人間可読 mutable ref (例: `16`, `16.4`, `latest`) または (b) immutable content ref (例: `sha256:...`) を許す。
- **`<ref>` が mutable か immutable かの判定は authority policy に委ねる** (`CAPSULE_URL_SPEC.md` の authority-specific policy 参照)。lock では必ず authority に query して immutable content ref に解決し、`resolved` フィールドに記録する。
- mutable ref を `dependencies.*.capsule` に書くこと自体は許す。lock がそれを fix する責務を持つ (Cargo の `^1.2` と同じ位置付け)。

### 4.2 例

| 形 | 意味 | 用途 |
| --- | --- | --- |
| `capsule://ato/postgres@16` | official registry, mutable major-version pin | 依存宣言の terse form |
| `capsule://ato/postgres@sha256:abc...` | content-addressed, immutable | lock 解決後の `resolved` |
| `capsule://github.com/Koh0920/WasedaP2P@<sha>` | source authority つき immutable | 通常の app capsule |

### 4.3 Grammar

```abnf
capsule-url   = "capsule://" authority "/" path "@" ref
authority     = ato-registry / dns-authority
ato-registry  = "ato"
dns-authority = labeled hostname per RFC 1035
ref           = mutable-ref / immutable-ref
mutable-ref   = 1*ALPHANUM-DOT          ; authority-specific syntax
immutable-ref = "sha256:" 64HEXDIG      ; content hash
```

## 5. `[dependencies.*]` Block Grammar

consumer 側 `capsule.toml`:

```toml
required_env = ["PG_PASSWORD"]            # manifest top-level。dep 内 {{env.X}} の resolution scope (§5.2)

[dependencies.<local-name>]
capsule    = "<capsule:// URL>"
contract   = "<contract-name>@<major>"

[dependencies.<local-name>.parameters]    # identity-bearing (instance_hash / dependency_derivation_hash 入り)
database = "wasedap2p"
encoding = "utf8"

[dependencies.<local-name>.credentials]   # runtime-only (identity 絶対除外、§7.3.1)
password = "{{env.PG_PASSWORD}}"          # template のまま lock 記録、resolve は orchestration 直前

[dependencies.<local-name>.state]
name      = "<state-name>"                # required if contract.state.required = true
ownership = "parent"                      # default "parent"。v1 で許される唯一値
```

- `<local-name>` は consumer manifest 内でユニークな alias。`needs = [...]` や `{{deps.<local-name>.runtime_exports.X}}` の参照キー。
- `<local-name>` は **identity に入らない** (§9.3)。
- `capsule` は §4 の URL。
- `contract` は §6 の `<name>@<major>`。
- `parameters` は §7.3 の provider 宣言と整合する key/value。**全て identity に入る**。
- `credentials` は §7.3.1 の provider 宣言と整合する key/value。**identity に絶対に入らない**。`{{env.X}}` テンプレを多用する。
- `state` は provider が `state.required = true` を宣言した時のみ必須。

### 5.1 Lifecycle Uniformity Invariant

> dependency capsule の起動 / ready 待ち / 停止は、通常の `ato run` と同一の実行モデル (`materialize → provision → start → wait → exports → stop`) で記述される。`managed` 等の二次 lifecycle 概念は外部仕様に出現しない。Ato 内部実装が既存 `ManagedServiceRuntime` 等を流用するのは構わないが、外部仕様の語彙には漏らさない。

これは **app / service / tool の違いはカテゴリではなく contract の違い** という Ato 哲学の言語化である。

### 5.2 Dep Env Resolution Scope

`[dependencies.<X>.parameters]` および `[dependencies.<X>.credentials]` の値文字列に出現する `{{env.<KEY>}}` テンプレは、以下のスコープでのみ resolve される:

```text
manifest top-level required_env ⊆ host environment
```

- **target-local `[targets.<t>.required_env]` は対象外**。target 自身の env 設定であり、dep parameter resolution には参加しない。
- 必要な env key は **manifest 直下 (top-level) の `required_env`** に明示宣言する。
- `[isolation].allow_env` には自動で追加されない (consumer が必要に応じて別途列挙)。
- lock 時 verification: `{{env.<KEY>}}` 出現 ∀ について `<KEY> ∈ top-level required_env` が成立。違反は lock 失敗。

これにより:
- dep parameter / credential は target start より前に resolve できる (target env 構築待ちにならない)
- 「target env と dep env の責務」が grammar で分離 (target は自身の env、dep は manifest-wide env)
- env 解決 scope が manifest からの読み取りだけで確定する (実行 phase 順序に依存しない)

具体例 (§11 worked example):
```toml
required_env = ["SECRET_KEY", "PG_PASSWORD"]    # manifest top-level

[targets.app]
required_env = ["SECRET_KEY"]                   # target が直接読む env のみ列挙

[dependencies.db.credentials]
password = "{{env.PG_PASSWORD}}"                # top-level required_env から拾う
```

## 6. Contract Identifier

### 6.1 Versioning

contract identifier は `<name>@<major>` 形式とする:

```text
service@1
service@2
tool@1
```

- `name` は Ato コアが定義する予約語 (v1 は `service` のみ実装、`tool` は予約)。
- `@<major>` は major version 整数。consumer は major まで pin する。

**Major bump 基準** (provider が contract block を変更したとき何を major bump とするか):

| 変更 | major bump |
| --- | --- |
| 既存 `parameters.<key>` を削除 | **必須** |
| 既存 `parameters.<key>.type` を変更 | **必須** |
| `parameters.<key>.required` を `false` → `true` | **必須** |
| 既存 `credentials.<key>` を削除 | **必須** |
| 既存 `credentials.<key>.type` を変更 | **必須** |
| `credentials.<key>.required` を `false` → `true` | **必須** |
| 既存 `identity_exports.<key>` を削除 | **必須** |
| 既存 `runtime_exports.<key>` を削除 | **必須** |
| `state.required` を `false` → `true` | **必須** |
| `state.required` を `true` → `false` | **必須** |
| `ready.type` を変更 | **必須** |
| `parameters.<key>` を `credentials.<key>` に移動 (or vice versa) | **必須** (identity に入るかが変わる、破壊的) |
| 新規 `parameters.<key>` を `required = false` で追加 | 不要 (minor 互換) |
| 新規 `credentials.<key>` を `required = false` で追加 | 不要 |
| 新規 `identity_exports.<key>` 追加 | 不要 |
| 新規 `runtime_exports.<key>` 追加 | 不要 |
| `parameters.<key>.default` の値変更 | 不要 (新値が古い consumer に流れるが互換) |
| `ready.timeout` の延長 | 不要 |

判定原則: **既存の lock を再 verify したときに失敗しうる変更は major bump 必須**。

### 6.2 v1 で固定する name

| name | 意味 | v1 status |
| --- | --- | --- |
| `service@1` | 長期実行 endpoint, ready probe + identity/runtime exports | **固定** |
| `tool@1` | 一回限り invoke, exit code で結果を返す | grammar 予約のみ。実行セマンティクスは follow-up |
| `app` | 通常の capsule の自己宣言 contract (= 既存の `targets.*`) | 暗黙、`[dependencies]` 経由では使わない |

## 7. Provider Side `[contracts."service@1"]`

provider capsule は実装する contract を `[contracts."<name>@<major>"]` block で宣言する。

### 7.1 全体構造

```toml
[targets.server]
runtime = "source"
driver  = "native"
run     = "postgres -D {{state.dir}} -k {{state.dir}} -p {{port}}"
port    = "auto"           # §7.5 TCP dynamic allocation (任意。endpoint を使わない provider もありうる)

[contracts."service@1"]
target = "server"          # §7.2 target binding
ready  = { type = "probe", run = "pg_isready -h 127.0.0.1 -p {{port}}", timeout = "30s" }

[contracts."service@1".parameters]            # identity-bearing (§7.3)
database = { type = "string", required = true }
encoding = { type = "string", default = "utf8" }

[contracts."service@1".credentials]           # runtime-only, identity 絶対除外 (§7.3.1)
password = { type = "string", required = true }

[contracts."service@1".identity_exports]
database = "{{params.database}}"
encoding = "{{params.encoding}}"
protocol = "postgresql"
major    = "16"

[contracts."service@1".runtime_exports]
PGHOST       = "{{host}}"
PGPORT       = "{{port}}"

[contracts."service@1".runtime_exports.DATABASE_URL]
value  = "postgresql://postgres:{{credentials.password}}@{{host}}:{{port}}/{{params.database}}"
secret = true                # §7.8 redaction-only flag (output 表示のみ。identity には影響しない)

[contracts."service@1".state]
required = true
version  = "16"               # §7.7 provider 主導の state schema version
mount    = "PGDATA"           # provider プロセスから見える env var 名
```

**識別の要点**: parameters は instance_hash を決め、credentials は決めない。`{{params.X}}` と `{{credentials.X}}` は別 namespace で、provider/consumer の双方が両者を混同できない grammar 設計。

### 7.2 Service Contract Target Binding

`[contracts."service@1"] target = "<label>"` で provider は **正確に 1 個** の `[targets.<label>]` を contract の実行 entry として binding する。

- `<label>` は provider manifest 内の `[targets.*]` の 1 entry を指す名前。
- 1 contract = 1 target。複数 target を 1 contract で起動することは v1 で禁止 (将来 follow-up)。
- provider が複数 target を持つ場合、binding されていない target は service dependency として起動されない (consumer が `ato run` で直接呼ぶ用途等)。
- lock 時 verification: `target` が provider の `[targets.*]` に存在すること **のみ**。

**`service@1` の本質は ready + exports contract** であり、endpoint allocation (`port` / `unix_socket`) は provider が ready/exports を達成する手段の 1 つにすぎない。endpoint を一切持たず、provider が `[ready] type = "probe"` で sentinel file を確認し、`runtime_exports` を file 由来の値で構成する形も合法。target に `port` も `unix_socket` も無いことを v1 verification は許す。

### 7.3 Parameters (identity-bearing)

provider は consumer から渡される **identity-bearing parameters** を `[contracts."service@1".parameters]` で宣言する。これは同一 provider capsule (例: `capsule://ato/postgres@16`) が複数 consumer に対して別々の **論理的な何か** (DB 名、スキーマ、protocol option 等) で機能するための grammar である。

- `type`: `string` / `int` / `bool` (v1 はこの 3 種のみ)
- `required`: bool (default `false`)
- `default`: parameter が省略された時の値 (型一致必須、required = true と排他)

consumer 側 `parameters.database = "wasedap2p"` がここに流し込まれる。

**Parameters は全て identity に入る** (hard invariant):
- `instance_hash` (state path key、§7.7) を決定する
- `dependency_derivation_hash` (v2 receipt identity field、§9.5) に JCS canonicalize して畳み込む
- run ごとに変わってはならない (deterministic)

#### 何を parameters に入れる/入れない

| 入れる (identity-bearing) | 入れない (= credentials へ、§7.3.1) |
| --- | --- |
| database 名、schema 名 | password、API token、OAuth secret |
| encoding、locale | TLS client cert (rotation 想定) |
| protocol major version | session token、JWT secret |
| 互換性に影響する config flag | host fingerprint (run-to-run drift) |

判定原則: **「これが変わったら state も変わるのが正しいか」を yes と答えられるなら parameters、そうでないなら credentials**。Postgres password が変わっても state.dir の中身は同じであるべき → credentials。

### 7.3.1 Credentials (runtime-only, identity 絶対除外)

provider は consumer から渡される **runtime credential** を `[contracts."service@1".credentials]` で宣言する。

```toml
[contracts."service@1".credentials]
password  = { type = "string", required = true }
api_token = { type = "string", required = false }
```

field 文法は `parameters` と概ね同じだが、**`default` field は v1 で禁止** (Safe by default の強化、後述):

| field | 許否 (parameters) | 許否 (credentials) |
| --- | --- | --- |
| `type` | OK (`string` / `int` / `bool`) | OK (同じ) |
| `required` | OK (default `false`) | OK (default `false`) |
| `default` | OK (literal 許容、identity 入り) | **禁止 (v1 lock 失敗)** |

identity 上の扱いの違い:

| 性質 | parameters | credentials |
| --- | --- | --- |
| `instance_hash` への寄与 | YES | **NO (hard invariant)** |
| `dependency_derivation_hash` への寄与 | YES | **NO (hard invariant)** |
| lock への記録 | resolved value | **template form のまま** (`"{{env.X}}"` を保存) |
| consumer manifest 記述位置 | `[dependencies.<X>.parameters]` | `[dependencies.<X>.credentials]` |
| テンプレ namespace | `{{params.<key>}}` | `{{credentials.<key>}}` |
| resolution timing | lock 時に literal 値も resolve 可 | **orchestration 直前** (provision / start / probe phase) |
| committed manifest 内 literal 許容 | YES | **NO** (lock 失敗) |

#### Hard invariants

1. credentials の値は **`instance_hash` 計算に決して使われない**。state path は credentials rotation で変わらない。
2. credentials の値は **`dependency_derivation_hash` 計算に決して使われない**。Pure/Closed 判定は credentials の値に影響されない。
3. credentials は consumer manifest で literal を直接書けない (= `password = "literal-secret"` は lock 失敗)。`{{env.X}}` テンプレ経由のみ。
4. credentials の env source key (`X` in `{{env.X}}`) は **manifest top-level `required_env`** に列挙されていなければならない (§5.2)。違反は lock 失敗。
5. lock 出力には credentials の **template 文字列をそのまま記録**。resolved value は lock 内に絶対に書かれない。
6. credentials の resolved value は orchestration 直前に env から拾い、provider process / runtime_exports に渡される。
7. **`credentials.<key>.default` の宣言は v1 lock 失敗** (provider manifest にも consumer manifest にも書けない)。

#### なぜ `default` を禁止するか (Safe by default rationale)

provider manifest に「既定パスワード」「既定 token」を書ける grammar を許すと:

- **既知 default の悪用**: provider capsule registry に publish された default password は誰でも参照可能 (`postgres / postgres` パターン)。consumer が override し忘れると本番で固定値が使われる事故が起きる。
- **identity 整合性の崩壊**: credential default を許すと「未指定の時の値」が provider の version に bake される。後で default を変更すると、consumer 側 lock を再生成しなくても挙動が変わる (実質的な silent breaking change)。
- **secret 管理の責務曖昧化**: default を提供すると「secret 管理は provider が責任を持つのか consumer が責任を持つのか」が両義的になる。Ato 哲学では **secret は consumer の host 由来 (env / secret-store) でしか入らない** が一貫した姿勢。

代替: credential を任意 (`required = false`) にしたい場合、provider は consumer 側で `[dependencies.<X>.credentials]` block 全体を省略可能とする (= credential 不在で動作する provider 実装にする)。何らかの「fallback 動作」を提供したいなら `parameters` 側で fallback policy flag を `bool` で表現する (これは identity に入る、= 設計判断として明示される)。

#### なぜ parameter の secret/identity = false flag ではなく separate block か

レビューで提示された代替案 (`parameters.<key>.identity_relevant = false`) より separate block が優れる理由:

- **mental model の負荷**: 「同じ block だが flag で挙動が変わる」より「block が違えば別物」が読みやすい
- **混入事故の防止**: identity-bearing と credential を 1 ブロックに混ぜると、後で provider author が flag を間違えると identity が drift する。block 分離なら配置ミスは type error で検出
- **template namespace の自然分離**: `{{params.X}}` vs `{{credentials.X}}` で参照側も明示
- **将来拡張**: secret-store から direct-fetch する `[credentials]` の sub-grammar (e.g., `password = { from = "vault:secret/path" }`) を追加する余地が生まれる

#### なぜ parameter の secret/identity = false flag ではなく separate block か

レビューで提示された代替案 (`parameters.<key>.identity_relevant = false`) より separate block が優れる理由:

- **mental model の負荷**: 「同じ block だが flag で挙動が変わる」より「block が違えば別物」が読みやすい
- **混入事故の防止**: identity-bearing と credential を 1 ブロックに混ぜると、後で provider author が flag を間違えると identity が drift する。block 分離なら配置ミスは type error で検出
- **template namespace の自然分離**: `{{params.X}}` vs `{{credentials.X}}` で参照側も明示
- **将来拡張**: secret-store から direct-fetch する `[credentials]` の sub-grammar (e.g., `password = { from = "vault:secret/path" }`) を追加する余地が生まれる

### 7.3.2 Credential Materialization Rules (v1 normative)

`{{credentials.X}}` テンプレを **plaintext textual substitution として展開することは v1 で禁止**。Ato runtime は以下のルールに従って materialize しなければならない (Safe by default の v1 規範):

#### Rule M1: argv / shell command 文字列への直接展開禁止

provider の `[targets.<label>] run = "..."` 文字列、`[provision] run = "..."` 文字列、`ready.run` 文字列の **argv / shell command body には `{{credentials.X}}` を素直に文字列置換してはならない**。

`{{credentials.X}}` が現れた場合、Ato runtime は以下のいずれかの **materialization channel** に置き換えてから process を spawn する:

| Channel | Materialization | 適用条件 |
| --- | --- | --- |
| **stdin** | `{{credentials.X}}` を含む command を実行する代わりに、credential 値を child process の stdin に書き込み、command 内の `{{credentials.X}}` は `/dev/stdin` か該当 fd を指すように rewrite | `run` が単一 command で stdin 経由読込を許容 |
| **temp file** | umask 077 で `<state.dir>/.ato-cred-<key>` (または tmpfs/anonymous fd) を作成、credential 値を 600 perm で書き込む。command 内の `{{credentials.X}}` を file path に rewrite。target/provision exit 後に **必ず unlink** | shell script / multi-command provision の主経路。command 列が複雑なときの第一選択 |
| **env var (provider 専用)** | provider process の env に `<key>=<value>` を inject (origin = `DepCredential`)。command 内の `{{credentials.X}}` を `${<key>}` に rewrite | `run` が env から読む明示的契約を持つ場合のみ |

- `temp file` channel が **v1 default**。実装が他 channel を選ぶには provider が明示的に opt-in する手段を v1.x で追加する。
- どの channel でも、`{{credentials.X}}` が argv に literal として現れる process invocation は **v1 lock 失敗** とする (= `{{credentials.X}}` のテンプレ表現が provider の `run` 文字列で literal interpolation 可能な位置に出現したら、parser が AST 上で channel marker に変換し、materialization を runtime に委譲する)。
- consumer 側の `[targets.<t>.env]` 等で `{{credentials.X}}` を直接書くことは禁止 (= `{{deps.<dep>.credentials.X}}` のような参照は **grammar に存在しない**)。consumer は `runtime_exports` 経由でのみ provider の credential 由来の値を間接的に受け取る。

#### Rule M2: credential を含む env / file は env capture から除外

Rule M1 の `env var` channel で provider に注入された credential、および `temp file` channel の file path 内容は、Ato 側の env capture / file observation から **無条件除外** する (origin = `DepCredential`、§7.4.1 の `EnvOrigin` に追加 variant)。

これにより:
- v2 receipt の `intrinsic_keys` に credential 値が混入しない
- v2 receipt の file observation に credential file の内容が記録されない
- explain-hash / replay 出力に credential 値が露出しない

#### Rule M3: redaction を log/error stream に強制

provider process の stdout / stderr が Ato runtime log に流れる時、Ato は **resolved credential 文字列を全て `***` に置換してから書き出す**。これは:
- runtime log
- v2 receipt の error message
- `ato explain-hash` の任意フィールド
- error stack trace
- session log file (`<session>.log`)

の全てに適用される。実装は credential resolution 時に値を redaction filter に register し、log writer / receipt builder の write path で必ず通す。

#### Rule M4: credential のメモリ / 露出境界

実装境界を明確にするため、v1 では以下の 2 階層に分けて規定する:

**M4-a: 必須保証 (v1 で実装が破ってはならない)**

resolved credential 値は以下のいずれにも **絶対に残らない / 出ない**:

| 経路 | 必須保証の内容 |
| --- | --- |
| lock file | template form のみ記録 (§7.3.1 Hard invariant 5) |
| v2 receipt | env capture / file observation から無条件除外 (§7.4.1 `EnvOrigin::DepCredential`)、redaction filter (Rule M3) を通過 |
| runtime log / session log | Rule M3 redaction filter を必ず通す |
| `ato explain-hash` 出力 | Rule M3 redaction filter を必ず通す |
| process argv (`ps aux` で見える) | Rule M1 channel 経由のみ (argv literal substitution 禁止、parser AST レベルで channel marker 化) |
| shell command body / shell history | Rule M1 channel 経由のみ |
| state.dir 外 file | Rule M1 temp file channel は `<state.dir>/.ato-cred-<key>` に限定、外側書き込みは Rule M5 lint で禁止 |

これらは v1 実装が **必ず守る** (= 1 つでも漏れたら CVE 級バグ)。

**M4-b: 努力義務 (v1 best-effort、v1.x で hard guarantee 化)**

resolved credential 値の Ato runtime プロセス内 memory における取り扱い:

```
host env -> resolver -> materialization channel -> child process -> drop
```

- credential 値を Rust `String` で長期保持しない
- materialization 完了後に source buffer をゼロクリア
- child process exit 後の cleanup で temp file を unlink

これは Rust `zeroize` 等のメモリ消去 crate でしか hard guarantee できない領域 (heap allocation の deterministic clearing、core dump 対策等)。v1 では「最善の努力」として実装し、v1.x の follow-up で `zeroize` 統合等で hard guarantee に格上げする。

> **整合性**: M4-a の「必須保証」は v1 で破ったら実装バグ、M4-b の「努力義務」は v1 で破っても実装は仕様準拠扱い (将来の hard guarantee 化に向けた指針)。レビューで指摘されたとおり、両者を混ぜると実装境界が曖昧になるため明示分離する。

#### Rule M5: provider author への明示的契約

provider author は以下を **遵守する責務がある** (Ato は parser / linter で検出可能な範囲は検証する):

- `[provision] run` で `{{credentials.X}}` を `echo` / `printf` への引数として書かない (process list 露出)
- shell variable expansion で credential を後段に渡さない (`PASSWORD={{credentials.X}}` 等は env channel を明示的に opt-in した時のみ)
- credential 値を ロギング目的で `>>` で file に流さない
- provider の `[provision]` script は credential 値を state.dir 外に書かない

これらは Ato lint で警告できる範囲は警告し、原則違反は v1 で **soft warn**、v1.x で fail-closed に強化する。

### 7.4 `identity_exports` vs `runtime_exports`

これは本 RFC の中核設計判断であり、Ato の identity モデル (`beyond-reproducible-build.md`, `IDENTITY_SPEC.md`) との一貫性を担保する **hard invariant** である。

**env 注入の主経路は `runtime_exports`**。`identity_exports` は info-only として消費するのが推奨パスで、env 注入は技術的に可能だが mental model を軽くするため第一選択にしない。

| | `identity_exports` | `runtime_exports` |
| --- | --- | --- |
| 値の決定タイミング | resolve 時 (parameters のみから) | 起動時 (parameters + credentials + 動的 host/port) |
| 値の入力 source | parameters のみ | parameters + credentials + 動的 endpoint 値 |
| run 間の安定性 | **deterministic** | **non-deterministic 許容** |
| `dependency_derivation_hash` への畳み込み | **YES** | **NO (hard invariant)** |
| consumer の env 注入 | 技術的に可能だが推奨しない (info-only として `{{deps.X.identity_exports.Y}}` で metadata 参照) | **YES (主用途)** |
| v2 receipt の `environment.intrinsic_keys` への計上 | 通常 env と同じ規則で計上可 | **必ず除外** (§7.4.1) |
| 例 | `protocol`, `database`, `major` | `DATABASE_URL`, `PGHOST`, `PGPORT` |

`{{credentials.X}}` を使えるのは `runtime_exports` のみ。`identity_exports` の値文字列に `{{credentials.X}}` を書いたら lock 失敗 (identity に credential を混ぜることになるため、parser で拒否)。

#### Hard invariant: runtime_exports は identity に入らない

`runtime_exports` の値が v2 receipt の identity hash に混入すると:

- `environment.mode = Closed` が永久に立たず Pure 到達不能
- 同一 capsule の連続実行で `dependency_derivation_hash` が drift し replay が壊れる

実装は env capture 時点で `runtime_exports` 由来の env entry を **identity-relevant env から除外** する責務を持つ (`execution_observers_v2.rs` 側)。この除外は consumer の `env_allowlist` より強い (allowlist に書かれていても除外される)。

#### 7.4.1 Env Capture Model 拡張要求 (origin tracking)

上記 hard invariant を実装するには、env capture model を **`(key, value)` の対から `(key, value, origin)` の三つ組に拡張** する必要がある。単なる key-allowlist では成立しない。

```rust
enum EnvOrigin {
    Host,                              // host process inherited
    ManifestStatic,                    // [targets.<x>.env] の literal 値
    ManifestRequiredEnv,               // required_env で host から 通過
    DepRuntimeExport(DepLocalName),    // [dependencies.<X>.runtime_exports.*] 由来
    DepIdentityExport(DepLocalName),   // [dependencies.<X>.identity_exports.*] 由来
    DepCredential(DepLocalName, CredKey),  // §7.3.2 Rule M1 の env channel で provider に注入された credential
}
```

- `DepRuntimeExport(_)` origin の entry は v2 receipt の `intrinsic_keys` 計算から **無条件除外**
- `DepIdentityExport(_)` origin の entry は通常の origin と同じ規則で計上 (deterministic、parent identity から導出可能)
- `DepCredential(_, _)` origin の entry は v2 receipt の `intrinsic_keys` および file observation から **無条件除外**、かつ Rule M3 redaction の対象
- 他の origin は既存規則を維持

これは `execution_observers_v2.rs` の env model 改修要求として `beyond-reproducible-build.md` の **environment closure 観測規約に対する破壊的でない拡張** (origin field の追加のみ) に位置付ける。既存 v2 receipt schema は kebab compatible (origin は internal model のみで receipt 表面には現れない)。

**v1 実装 invariant**:
1. env injection 時に origin を tag し続ける (consumer process 起動 → 子 env tag → 観測 → 識別計算、の経路全体で)
2. `intrinsic_keys` 計算は origin = `DepRuntimeExport` を **必ず** 除外する
3. consumer の `env_allowlist` は origin tag を override できない (allowlist は host/manifest 由来 env 用)

### 7.5 Dynamic Endpoint Allocation (TCP only in v1)

provider の `[targets.<label>]` は次の auto allocation を許す:

```toml
port         = "auto"     # TCP port を Ato が割当 → {{port}} に展開 (v1 実装対象)
unix_socket  = "auto"     # parser は AST 受理、lock verification で fail-closed (§9.1)
host         = "127.0.0.1" # 固定値推奨。v1 は実質これのみ
```

- `port = "auto"` の TCP port は起動時に未使用ポートから 1 個割当。consumer の起動が完了するまで予約。
- 複数 service dep が同 graph 内に存在する場合、各々独立に allocation される (ポート衝突は Ato が解決)。
- allocated 値はテンプレ変数 `{{host}}` / `{{port}}` で `runtime_exports` から参照される。`identity_exports` からは参照禁止 (deterministic 違反)。
- endpoint allocation を **使わない provider** も合法 (§7.2)。`port` も `unix_socket` も宣言せず、provider が自前 IPC で ready/exports を達成する形を v1 は許す。

**v1 における `unix_socket = "auto"` の境界**: parser は AST に受理するが、**lock verification phase で必ず fail** する (§9.1 verification 13)。「parser accepts / runtime rejects」ではなく「parse OK / lock fail-closed」と読むこと。「lock できたが起動できない」状態を作らないのが v1 invariant。grammar 予約は後方互換のため。`unix_socket` の race condition / path stability / sandbox 統合は v1.x の follow-up `CAPSULE_DEPENDENCY_UNIX_SOCKET` で扱う。

### 7.6 `ready` Probe Sum Type

ready probe は最初から sum type として宣言する。v1 で grammar 予約する 4 variant のうち、**v1 で動く (= lock を通過する) のは `tcp` と `probe` のみ**。`http` / `unix_socket` は **parse は通るが lock verification で必ず fail する** (§9.1 verification 13):

```toml
ready = { type = "tcp",         target = "{{host}}:{{port}}", timeout = "30s" }    # v1 実装対象、lock 通過
ready = { type = "probe",       run = "pg_isready -h {{host}} -p {{port}}", timeout = "30s" }  # v1 実装対象、lock 通過
ready = { type = "http",        url = "http://{{host}}:{{port}}/health", expect_status = 200, timeout = "30s" }  # parse OK / lock fail-closed
ready = { type = "unix_socket", path = "{{socket}}", timeout = "30s" }              # parse OK / lock fail-closed
```

後で増えても破壊的でない。`http` / `unix_socket` は v1.x follow-up で grammar 予約から「lock 通過」へ昇格する。「lock できたが起動できない」状態を作らないのが v1 invariant。

**`probe` 実行 context** (v1 で実装する probe variant):
- cwd: provider target の `working_dir` または provider capsule root
- env: provider target の env (consumer の env は注入しない)
- stdin: 閉じる
- stdout/stderr: capture して runtime log へ
- exit code 0 = ready、非 0 = まだ ready ではない (timeout 内 retry)
- timeout 超過 = readiness failure として consumer 起動を中止

**`tcp` 実行 context**:
- 短時間 connect を試行、確立したら即 close
- 失敗時は backoff retry (実装裁量、v1 は固定間隔可)
- timeout 超過 = readiness failure

### 7.7 State 宣言と Path Rule

provider:

```toml
[contracts."service@1".state]
required = true            # consumer は state.name を渡す必要がある
version  = "16"            # state schema version。provider 主導で「state 互換が壊れる単位」を宣言
mount    = "PGDATA"        # provider プロセスから見える env var 名 (provider 起動時に inject)
```

`version` は contract major でも provider capsule major でもなく、**provider が自分の state の互換性境界として宣言する独立な値**。例:
- `postgres@16.4` と `postgres@16.5` は patch 違いだが state 互換 → 両方 `state.version = "16"`
- `postgres@17.0` は data 互換が壊れる → `state.version = "17"`、新 state.dir で fresh start

これにより `service@1` contract version、provider capsule version、state version の 3 つが独立に進化できる。

#### v1 path rule

```
<ato-home>/state/<parent-package-id>/<dep-instance-hash>/<state-version>/<state-name>/
```

- `<parent-package-id>` = consumer capsule の Ato package id (existing concept)
- `<dep-instance-hash>` = `blake3-128(JCS({"resolved": ..., "contract": ..., "parameters": ...}))[:16]` の hex prefix。alias / credentials / runtime_exports / identity_exports は入力に **入らない** (§9.3)。credentials を入れない invariant により、credential rotation で state path が変わらないことが構文的に保証される
- `<state-version>` = provider の `[contracts."service@1".state] version` 値
- `<state-name>` = consumer の `[dependencies.<X>.state] name`

この path 規則の含意:

- **alias rename は state を保つ**. consumer が `[dependencies.db]` を `[dependencies.database]` に rename しても、`(resolved, contract, parameters)` が変わらなければ instance hash は同一 → 同 state path → state 継続。alias は consumer-side 表示だけの存在となる。
- **credential rotation は state を保つ**. consumer が `credentials.password` の env source を rotate しても、credentials は instance hash の入力に入らないので path 不変 → state 継続。`PG_PASSWORD` を rotate して fresh DB に見える危険を構文で排除。
- **per-parent isolation**. 別 parent capsule が同 provider を同 parameters で参照しても `<parent-package-id>` で分離。
- **per-instance**: 同 parent 内で `(resolved, contract, parameters)` の異なる dep は path が分かれる。同一なら §9.3 で lock 失敗 (path 共有問題は構文時点で排除)。
- **per-state-version**: provider が `state.version` を bump すると新ディレクトリが切られる。**v1 では Ato は自動 migration を提供しない**。provider 側で migration logic を `[provision]` に書くか、ユーザに manual migration を任せる。

#### state.dir の Ato 側保証

- 起動時に存在しなければ `mkdir -p`
- パーミッションは parent と同一
- GC: parent uninstall 時に `parent-isolated` state は GC 候補 (実際の削除タイミングは `ATO_HOME_LAYOUT.md` の GC policy に従う)
- parent の state directory に置かれるため、host のグローバル namespace を汚染しない

**`ownership = "shared"` は v1 で parser reject** (予約済 keyword)。

### 7.8 Secret Handling (overview)

v1 における secret 取り扱いの全体像は以下の 3 層に整理される。各層の規範本文は別 section にあり、本 section はその index として機能する:

| 層 | 何を担保するか | 規範本文 |
| --- | --- | --- |
| **Grammar 層** | secret 値は `[dependencies.*.credentials]` (consumer) / `[contracts.*.credentials]` (provider) でしか宣言できず、identity / state path に絶対に入らない | §7.3.1 (Credentials block)、§9.5 (identity 畳み込み)、§9.3 (instance hash) |
| **Materialization 層** | resolved credential 値は plaintext substitution されず、stdin / temp file / env channel のいずれかで provider process に渡される。argv / shell command body には現れない | **§7.3.2 (Credential Materialization Rules M1–M5)** |
| **Output 層** | logs / receipt / explain-hash / error message から credential 値が redact される | §7.3.2 Rule M3、本 section の `runtime_exports.<key>.secret = true` flag |

#### `runtime_exports.<key>.secret = true` (output redaction)

provider が `runtime_exports` の特定 key を redaction 対象としてマークする flag:

```toml
[contracts."service@1".runtime_exports.DATABASE_URL]
value  = "postgresql://postgres:{{credentials.password}}@{{host}}:{{port}}/{{params.database}}"
secret = true
```

- `secret = true` の値は logs / receipt / `ato explain-hash` 出力 / error message から redact される (Rule M3 と同じ redaction filter を通る)。
- env injection 時に target process が見る値は実値 (redaction は表示用途のみ)。
- `runtime_exports` は §7.4 で既に identity 除外なので、`secret = true` flag は **redaction policy の宣言のみ** であり、identity 計算には一切影響しない。
- `runtime_exports.<key>` の値文字列に `{{credentials.X}}` が含まれる場合、parser は `secret = true` の **暗黙適用を推奨** (provider author が明示し忘れた場合の safety net)。v1 では soft warn、v1.x で auto-apply に格上げ予定。

#### v1 で意図的に予約しない構文

- `parameters.<key>.secret = true` (provider 側): credentials block と分離した今は不要。secret は grammar として credentials block に集約される。
- `[dependencies.*.parameters].<key>` の literal-rejection: parameters は identity-bearing で literal が許容される (= committed manifest に書かれる前提)。secret は credentials へ。

#### Follow-up

- `CAPSULE_DEPENDENCY_SECRET_IDENTITY`: hash-of-value 方式で credential も cross-host replay の identity に取り込む拡張。v1 では credential は identity 完全除外なので cross-host replay は credential については保証されない。
- `CAPSULE_DEPENDENCY_CREDENTIAL_ROTATION`: 既存 state を保ったまま credential rotation を provider に通知する hook。

## 8. `dependencies` vs `needs`

両者は別概念:

| | `dependencies` | `needs` |
| --- | --- | --- |
| 何を表すか | **何に依存しているか** (identity / lock 対象) | **target 起動前に ready であるべきもの** (orchestration order) |
| 書く場所 | `[dependencies.<name>]` (top level) | `targets.<label>.needs = [...]` |
| `tool@1` 依存 | 書く | 書かない (provision/build 時のみ) |
| `service@1` 依存 | 書く | 書く (ready 待ちが要る) |

**Integrity rule** (lock 時 verify):

- `needs` に列挙された名前は **全て `[dependencies.<X>]` に存在しなければならない**。
- `[dependencies.<X>]` にあって `needs` に無い `service@1` 依存は許容される (provision 用途、または explicit lazy start)。ただし v1 では `service@1` を実際に起動するのは `needs` に列挙された場合のみ (lock 時警告)。
- 循環は lock 失敗 (§9)。

例:

```toml
[dependencies.codegen]                                      # tool dep: lock するが needs に出さない
capsule  = "capsule://ato/openapi-generator@7"
contract = "tool@1"

[dependencies.db]                                           # service dep: lock + needs 両方
capsule  = "capsule://ato/postgres@16"
contract = "service@1"
parameters = { database = "wasedap2p" }

[dependencies.db.state]
name = "db"

[targets.app]
needs = ["db"]
env.DATABASE_URL = "{{deps.db.runtime_exports.DATABASE_URL}}"
```

## 9. Lock-Time Verification

resolve / lock フェーズで以下を検証する。**実行時ではなく lock 時に落とす** のが要点 (Cargo の `Cargo.lock` 生成と同じ責務分離)。

### 9.1 Lock-time に行う検証

1. **Capsule resolution**. `capsule` URL の mutable ref を authority に query して immutable content ref に解決し、`resolved` に書く。
2. **Provider fetch**. resolved capsule を fetch (キャッシュ可) し、manifest を読める状態にする。**provider fetch せずに lock を完了することはできない**。
3. **Contract presence**. consumer の `contract = "service@1"` に対し、resolved capsule が `[contracts."service@1"]` block を持つこと。
4. **Target binding existence**. `[contracts."service@1"] target = "<label>"` の `<label>` が provider の `[targets.*]` に存在すること。endpoint (port / unix_socket) 宣言は **要求しない** (§7.2)。
5. **Parameter validation**. consumer の `parameters` が provider の宣言と型・required で一致。`required = true` 未指定なら lock 失敗。型不一致 lock 失敗。
6. **Credentials validation**. consumer の `credentials` が provider の宣言と型・required で一致 (§7.3.1)。違反は lock 失敗。consumer が `credentials.<key>` に literal を書いた (= `{{env.X}}` テンプレで囲まれていない) 場合は lock 失敗。`{{env.<KEY>}}` の `<KEY>` が manifest top-level `required_env` に列挙されていなければ lock 失敗 (§5.2)。**provider または consumer が `credentials.<key>.default` を宣言していたら lock 失敗** (§7.3.1 Hard invariant 7、Safe by default)。
7. **Identity_exports purity**. provider の `identity_exports.<key>` の値文字列に `{{credentials.X}}` が出現したら lock 失敗 (identity に credential が混ざるのを構文で禁止、§7.4)。
8. **State requirement**. provider の `state.required = true` の時、consumer が `[dependencies.<name>.state] name` を指定していること。`ownership = "shared"` は lock 失敗 (v1)。provider が `state.version` を宣言していなければ lock 失敗 (v1 で `state.required = true` の必須 field)。
9. **needs ⊆ dependencies**. `needs` の全名前が `[dependencies.*]` に存在。違反は lock 失敗。
10. **Cycle detection**. `needs` graph に循環があれば lock 失敗。
11. **Major-version uniqueness**. 同 graph 内で同 capsule の異 major が現れたら lock 失敗 (v1 制約)。
12. **Alias-instance uniqueness** (§9.3). 同 graph 内で `(resolved, contract, parameters)` が一致する dep が 2 つ以上あれば lock 失敗 (v1)。credentials は uniqueness key に入らない (= 同 dep を異 credential で 2 個書いても lock 失敗)。
13. **Reserved variant rejection (fail-closed)**. `unix_socket = "auto"`, `ready.type = "http"`, `ready.type = "unix_socket"` のいずれかが contract / target に出現したら **lock 失敗** (v1 では runtime 未実装。「lock できたが起動できない」状態を作らない)。grammar 予約 = parser は AST に受理する、しかし lock 出力には到達させない。

### 9.2 Runtime-time に行う検証

lock 時には判定不能で、起動時に落ちるもの:

- ready probe success (provider が `ready.timeout` 内で ready にならない)
- endpoint allocation success (`port = "auto"` で空きポートが取れない等)
- state.dir initialization (provision script の冪等性は provider 責務、§10.1)
- consumer target の `wait_for_http_ready` 成功
- orphan detection (`.ato-session` sentinel の owner pid が dead で残存する場合の warn、§10.4)

ここで失敗した場合は v2 receipt に記録される (cf. `execution_receipt_builder.rs`)。lock 自体は invalidate されない (再起動で復帰可能性があるため)。

### 9.3 Alias と Instance Identity

- `<local-name>` は **identity にも state path にも入らない**。consumer-side の表示名にすぎない。
- `dep-instance-hash` (= state path の version segment 直前の identifier、§7.7) は `blake3-128(JCS({"resolved": ..., "contract": ..., "parameters": ...}))[:16]` で導出する。**alias / credentials / runtime_exports / identity_exports は入力に一切入らない**。
- `dependency_derivation_hash` (= v2 receipt の identity field) は alias を sort key に使わず、`(resolved, contract, parameters, identity_exports)` の tuple で sort + JCS canonicalize する。state version は入れない (provider 自身の resolved hash に既に焼き込まれているため)。**credentials は入らない**。
- **同一 graph 内で `(resolved, contract, parameters)` が一致する dep が 2 つ以上あったら lock 失敗** (v1)。credentials の違いは uniqueness key として効かない (= 同じ DB を 2 つの password で接続する意味の宣言は禁止、credentials を別にしたいなら parameters も別にする)。これにより「1 instance か 2 instance か」の曖昧性も「同 instance hash → 同 state path 競合」も構文上排除する。後で shared instance を許す follow-up RFC を書く時に backward compatible。

#### Alias rename と state continuity

```toml
# Before
[dependencies.db]
capsule = "capsule://ato/postgres@16"
contract = "service@1"
parameters = { database = "wasedap2p" }
[dependencies.db.state]
name = "data"

# After (alias rename only)
[dependencies.database]
capsule = "capsule://ato/postgres@16"
contract = "service@1"
parameters = { database = "wasedap2p" }
[dependencies.database.state]
name = "data"
```

`<local-name>` が `db` → `database` に変わっても `(resolved, contract, parameters)` は不変なので **同じ instance hash → 同じ state path** → state は連続。consumer manifest 内の `{{deps.db.runtime_exports.X}}` を `{{deps.database.runtime_exports.X}}` に書き換えれば動作する。

例 (lock 失敗):

```toml
[dependencies.db_a]
capsule    = "capsule://ato/postgres@16"
contract   = "service@1"
parameters = { database = "shared" }

[dependencies.db_b]                              # 同じ capsule + 同じ parameters → 失敗
capsule    = "capsule://ato/postgres@16"
contract   = "service@1"
parameters = { database = "shared" }
```

`parameters.database` を別の値にするか、片方を削除して `db_a` のみで参照する。

### 9.4 Lock 出力形

```json
{
  "schema_version": "1",
  "dependencies": {
    "db": {
      "requested": "capsule://ato/postgres@16",
      "resolved":  "capsule://ato/postgres@sha256:abc123...",
      "contract":  "service@1",
      "parameters": {
        "database": "wasedap2p",
        "encoding": "utf8"
      },
      "credentials": {
        "password": "{{env.PG_PASSWORD}}"
      },
      "identity_exports": {
        "database": "wasedap2p",
        "encoding": "utf8",
        "protocol": "postgresql",
        "major":    "16"
      },
      "state": {
        "name":      "db",
        "ownership": "parent",
        "version":   "16"
      },
      "instance_hash": "blake3:7f4a..."
    }
  }
}
```

- `parameters` は resolved value で記録 (identity に入る)。
- `credentials` は **template form のまま記録** (resolved value は lock に絶対入らない)。
- `runtime_exports` / `<local-name>` は lock の identity 部分に書かない (前者は run-to-run drift、後者は alias)。
- `instance_hash` は §7.7 の path key 兼 §9.3 の uniqueness key (`(resolved, contract, parameters)` のみから導出、credentials を含まない)。

### 9.5 Identity 畳み込み

`dependency_derivation_hash` (v2 receipt 既存 field) は以下の JCS canonical form を blake3-256 で hash する:

```
[
  {
    "resolved":          "<capsule://...@sha256:...>",
    "contract":          "<name>@<major>",
    "parameters":        { ... sorted keys },
    "identity_exports":  { ... sorted keys }
  },
  ...                                                   ; (resolved, contract, parameters) で sort
]
```

- `requested` は入れない (mutable ref は identity ノイズ)。
- **`credentials` は入れない** (hard invariant、§7.3.1)。
- `runtime_exports` は入れない (run-to-run drift, hard invariant)。
- `state.name` / `state.version` は入れない (`resolved` capsule hash に焼き込まれており重複)。
- `<local-name>` は入れない (alias)。

`instance_hash` (state path key、§7.7) は同じ tuple から `identity_exports` を **除いた** subset で計算する:

```
blake3-128(JCS({"resolved": ..., "contract": ..., "parameters": ...}))[:16]
```

`identity_exports` が `instance_hash` に入らない理由: provider が `identity_exports.<key>` を後で追加 (= minor 互換、§6.1) した時、state path が変わってしまうのを避ける。`identity_exports` は consumer 側の identity に入るが、provider 側 state ID には不要。

> **Note**: transitive dependency (provider 自身が更に dep を持つ場合) の hash 畳み込みは **本 RFC v1 の対象外**。Phase 2 reconstruct と合わせて follow-up RFC で扱う。v1 では direct deps のみが parent receipt の identity に入る。

## 10. Runtime Semantics

### 10.1 Provision の冪等性

provider capsule の `[provision]` block は **複数回呼ばれうる** (consumer の最初の起動 + state corruption 後の修復等)。

**v1 規約**:
- provision の冪等性は **provider 責務**。Ato は provision phase の成功/失敗 sentinel を管理しない。
- provider は state.dir 内の自前 sentinel (例: `<state.dir>/.initialized`) を見て早期 return する慣習を推奨。
- partial failure 後の state は provider の判断で healing。Ato は外側からは「provision が成功して target 起動段階に進めた」かしか見ない。

理由: Ato が sentinel を持つと provision の成功定義が capsule から漏れる。provider が自前で持てば「自分にとって何が completed か」を自分で定義できる。

### 10.2 起動シーケンス

consumer `ato run` の流れ:

1. lock を読む (`dependencies.*` を topological sort)
2. 各 service dep について:
   1. provider capsule を materialize (cache hit ならスキップ)
   2. state.dir 確保 (§7.7 path rule、instance hash から導出)
   3. credential resolution: lock の credentials template (`{{env.X}}`) を host env から resolve (scope = manifest top-level `required_env`、§5.2)、メモリ上保持
   4. **orphan / sentinel check (§10.4 の 4-state 表に従う)**:
      - sentinel 不在 → 通常 start、新規 sentinel 書き込み
      - sentinel あり / owner pid dead → warn + sentinel 削除 + 通常 start
      - sentinel あり / 同一 Ato session → resume start (再 ready check のみ)
      - sentinel あり / 別 alive Ato session → **warn + abort** (data-dir 競合の安全性のため、本 dep の起動を中止)
   5. provider の `[provision]` を実行 (provider 内部で冪等性管理、credential は §7.3.2 Rule M1 の materialization channel 経由)
   6. provider target を start (`port = "auto"` の TCP allocation 等、credential は同 channel で provider process に到達)
   7. `ready` probe を実行 (timeout 内)
   8. `runtime_exports` を resolve (allocated host/port + credential をテンプレ展開、env origin = `DepRuntimeExport(<dep-local-name>)` でタグ。`secret = true` 値は redaction filter に register)
3. consumer target を start。env に `runtime_exports` 由来の値を注入 (origin tag 維持)
4. consumer の通常 `wait_for_http_ready` (consumer 自身の port)
5. consumer 終了を待つ

### 10.3 Endpoint Allocation の Lifecycle

`port = "auto"` で割り当てられた port は:
- consumer target start の直前に予約 (TCP listen socket を Ato が短時間握って `runtime_exports` 展開後に provider に渡す、または OS assigned port パターン)
- provider が listen 開始 → consumer に渡る → consumer 終了 → provider 停止 で release
- 同 port を別 consumer が使うことは衝突しない (consumer ごとに独立 allocation)

allocation race の具体的解決方式 (短時間握る vs OS-assigned vs `<state.dir>/.port` ファイル経由) は §13 Open Question に残る。v1 実装は最初に動く方式を採用し、`runtime_exports` テンプレ展開 timing が race-free であることを保証する。

`unix_socket = "auto"` は v1 で grammar 予約のみ、runtime 未実装 (§7.5)。

### 10.4 Teardown Ordering と Orphan Detection

**Teardown** (consumer の通常終了 / Ctrl-C / crash):

1. consumer target に SIGTERM
2. consumer 終了確認 (timeout 後 SIGKILL)
3. service deps を **reverse topological order** で停止 (consumer から見た直接 deps から先に)
4. 各 dep に SIGTERM → timeout → SIGKILL
5. state.dir 内の sentinel `.ato-session` を削除

**v1 制約**: 1 service instance を共有する複数 consumer は禁止 (§2)。よって refcount は不要。

**Background mode** (`ato run -b`):
- service deps は consumer session が active な間だけ生存
- session が detach (background pid 継続) しても deps は session に紐づく (Ato が session pid を sentinel に書く)
- session 終了 = deps 全停止

**Orphan detection (v1: warn-only)**:

Ato は service start 時に `<state.dir>/.ato-session` に owner Ato session pid を書く。次の `ato run` でその dep を起動しようとした時:

| 状態 | v1 アクション |
| --- | --- |
| sentinel 不在 | 通常 start。新規 sentinel を書く |
| sentinel あり、owner pid が **dead** | **warn** + sentinel を Ato が削除 + 通常 start (provider の data-dir lock が残存プロセスを排除する前提) |
| sentinel あり、owner pid が **alive で同一 Ato session** | resume start (再 ready check のみ) |
| sentinel あり、owner pid が **alive で別 Ato session** | **warn** + start を中止 (data-dir 競合の安全性は保証できないため) |

**v1 では auto-kill / auto-GC は実装しない**。`ATO_HOME_LAYOUT.md` の GC policy が将来 kill/cleanup を実装する余地は残すが、本 RFC v1 の範囲は detection + warn まで。

provider の data-dir locking (Postgres `postmaster.pid` 等) が存在する想定で v1 は安全。data-dir locking を持たない provider が orphan で corruption を起こすリスクは provider 側の責任とする。

## 11. Worked Example: WasedaP2P + ato/postgres

### 11.1 Consumer (`WasedaP2P/capsule.toml`)

```toml
schema_version = "0.3"
name           = "wasedap2p-backend"
version        = "0.1.0"

# manifest top-level required_env: dep parameter / credential resolution の env scope (§5.2)
required_env = ["SECRET_KEY", "PG_PASSWORD"]

[dependencies.db]
capsule  = "capsule://ato/postgres@16"
contract = "service@1"

[dependencies.db.parameters]                     # identity-bearing
database = "wasedap2p"

[dependencies.db.credentials]                    # runtime-only, identity 除外 (§7.3.1)
password = "{{env.PG_PASSWORD}}"

[dependencies.db.state]
name = "db"

[targets.app]
runtime         = "source"
driver          = "python"
runtime_version = "3.11.10"
working_dir     = "backend"
run             = "python -m uvicorn main:app --host 127.0.0.1 --port 8000"
port            = 8000
needs           = ["db"]
required_env    = ["SECRET_KEY"]                 # target が直接読む env のみ (PG_PASSWORD は dep 用)

[targets.app.env]
ALGORITHM    = "HS256"
DATABASE_URL = "{{deps.db.runtime_exports.DATABASE_URL}}"
FRONTEND_URL = "http://localhost:5173"

[isolation]
# allow_env は target が host から直接 inherit する env を限定する既存仕様。
# - SECRET_KEY: target.app が直接読む。target env injection あり。
# - PG_PASSWORD は意図的に **含めない**: dep credential resolver が manifest top-level required_env から
#   読み取り Ato runtime のメモリに保持し、provider process に §7.3.2 materialization channel で渡す。
#   target.app の env / process には PG_PASSWORD は inject されない (= target.app から `os.environ["PG_PASSWORD"]` で
#   読めない)。target が password 値を欲しい場合は `runtime_exports.DATABASE_URL` に埋め込まれた形で受け取る。
allow_env = ["SECRET_KEY"]
```

ポイント:
- `password` が `[dependencies.db.credentials]` に分離されている → `PG_PASSWORD` rotation は instance_hash を変えず、Postgres data dir は同じ DB を指し続ける。
- top-level `required_env = [..., "PG_PASSWORD"]` が dep credential resolution の env scope を提供 (§5.2)。target-local `required_env` は target が自分で読む env のみ。
- `[isolation].allow_env` には `PG_PASSWORD` を **含めない**。これが本仕様の重要な違いで、credential は target env を経由せず provider process まで届く (§7.3.2 Rule M1)。consumer target は password を直接見ず、`{{deps.db.runtime_exports.DATABASE_URL}}` 経由で接続文字列を受け取る。
- `DATABASE_URL` の hardcode は消え、host 側で Postgres を立てておく必要も消える。

### 11.2 Provider (`ato/postgres/capsule.toml`, 概念例)

```toml
schema_version = "0.3"
name           = "postgres"
version        = "16.4"

[targets.server]
runtime  = "source"
driver   = "native"
run      = "postgres -D {{state.dir}} -k {{state.dir}} -p {{port}}"
port     = "auto"
host     = "127.0.0.1"

[provision]
# §7.3.2 Rule M1 (temp file channel) を使う。
# {{credentials.password}} は AST 上で channel marker。Ato runtime が:
#   1. umask 077 で <state.dir>/.ato-cred-password に 600 perm で値を materialize
#   2. provision script 内の {{credentials.password}} を file path に rewrite
#   3. script exit 後に必ず unlink + memory zeroize
# よって provision 本文には credential 値が plaintext として現れない (process list / shell history / debug log に出ない)。
run = """
  set -eu
  if [ ! -f {{state.dir}}/.initialized ]; then
    initdb -D {{state.dir}} --encoding={{params.encoding}} --auth-local=password --pwfile={{credentials.password}} --username=postgres
    pg_ctl -D {{state.dir}} -l {{state.dir}}/log start -o "-p {{port}} -k {{state.dir}}"
    createdb -h 127.0.0.1 -p {{port}} -U postgres {{params.database}}
    pg_ctl -D {{state.dir}} stop
    touch {{state.dir}}/.initialized
  fi
"""
# 注: {{credentials.password}} は file path に展開される (Rule M1 temp file channel)。
# `printf` で argv に書く / `echo "{{credentials.password}}"` で history に残す等のパターンは
# Ato lint で警告される (Rule M5)。

[contracts."service@1"]
target = "server"
ready  = { type = "probe", run = "pg_isready -h 127.0.0.1 -p {{port}}", timeout = "30s" }

[contracts."service@1".parameters]              # identity-bearing
database = { type = "string", required = true }
encoding = { type = "string", default = "utf8" }

[contracts."service@1".credentials]             # runtime-only, identity 除外 (§7.3.1)
password = { type = "string", required = true }

[contracts."service@1".identity_exports]
database = "{{params.database}}"
encoding = "{{params.encoding}}"
protocol = "postgresql"
major    = "16"
# 注: identity_exports に {{credentials.X}} は書けない (§7.4 / §9.1 verification 7)

[contracts."service@1".runtime_exports]
PGHOST = "{{host}}"
PGPORT = "{{port}}"

[contracts."service@1".runtime_exports.DATABASE_URL]
value  = "postgresql://postgres:{{credentials.password}}@{{host}}:{{port}}/{{params.database}}"
secret = true                                  # redaction-only flag (§7.8)

[contracts."service@1".state]
required = true
version  = "16"                                # state schema version (§7.7)
mount    = "PGDATA"
```

provision の credential 経路について (§7.3.2 適用結果):

| 露出経路 | v1 における防護 |
| --- | --- |
| 文字列 textual substitution | Rule M1: `{{credentials.password}}` は AST 上で channel marker、plaintext 文字列置換しない |
| process list (`ps aux`) の argv | Rule M1: argv に literal で出ない (temp file path に rewrite) |
| shell history | Ato が exec する script は履歴に残らない + Rule M5: `echo {{credentials.X}}` パターンを lint で警告 |
| temp file の漏洩 | Rule M1: state.dir 内、umask 077 で 600 perm、provision 完了直後に Ato runtime が unlink |
| stdout / stderr / runtime log | Rule M3: redaction filter が resolved 値を `***` に置換してから write |
| v2 receipt の env capture | §7.4.1: origin = `DepCredential` で intrinsic_keys から無条件除外 |
| `runtime_exports.DATABASE_URL` 内の credential 部分 | `secret = true` で Rule M3 redaction が表示出力に適用。target process が見る env value は実値 (注入経路は §7.3.2 channel) |

これは「v1 RFC は secret を grammar (credentials block) と materialization (Rule M1–M5) の二層で特別扱いする」「provider author が誤って漏らせる経路を Ato 側で塞ぐ」という Safe by default の v1 規範。

### 11.3 Lock 出力 (consumer 側)

```json
{
  "schema_version": "1",
  "dependencies": {
    "db": {
      "requested": "capsule://ato/postgres@16",
      "resolved":  "capsule://ato/postgres@sha256:...",
      "contract":  "service@1",
      "parameters": {
        "database": "wasedap2p",
        "encoding": "utf8"
      },
      "credentials": {
        "password": "{{env.PG_PASSWORD}}"
      },
      "identity_exports": {
        "database": "wasedap2p",
        "encoding": "utf8",
        "protocol": "postgresql",
        "major":    "16"
      },
      "state": { "name": "db", "ownership": "parent", "version": "16" },
      "instance_hash": "blake3:7f4a..."
    }
  }
}
```

- `parameters` は resolved value で記録 → identity 計算入り。
- `credentials.password` は **template `"{{env.PG_PASSWORD}}"` のまま** lock に書かれる。resolved value は lock に書かれない。`PG_PASSWORD` を rotate しても lock は変わらず、よって `instance_hash` も `dependency_derivation_hash` も変わらない。
- `instance_hash` は §7.7 path key 兼 §9.3 uniqueness key (`(resolved, contract, parameters)` のみから導出、credentials を含まない)。

### 11.4 起動シーケンスと state path

- instance_hash 計算入力: `{"resolved": "capsule://ato/postgres@sha256:...", "contract": "service@1", "parameters": {"database": "wasedap2p", "encoding": "utf8"}}`。**`credentials.password` は入らない**。
- state.dir = `<ato-home>/state/<wasedap2p-backend pkg id>/<instance_hash>/16/db/`
- credential resolution: `{{env.PG_PASSWORD}}` を host env から拾い (top-level `required_env` で宣言済)、メモリ上に保持
- orphan check: `<state.dir>/.ato-session` が無い → 通常 start (§10.4)
- provision: `initdb` + `createdb wasedap2p` を `.initialized` sentinel で 1 回限り。pwfile は umask 077 で短命、provision 完了で unlink (§11.2 secret-safe pattern)
- target start: `postgres -D <state.dir> -k <state.dir> -p 49172` (例)
- ready: `pg_isready -h 127.0.0.1 -p 49172` を timeout 内で成功
- runtime_exports resolve: `DATABASE_URL = postgresql://postgres:***@127.0.0.1:49172/wasedap2p` (secret = true なので logs では redact、target には実値が渡る)
- consumer 起動: env に `DATABASE_URL` 注入 (origin = `DepRuntimeExport("db")`)。consumer は通常通り uvicorn 起動
- v2 receipt env capture (§7.4.1): `DATABASE_URL` は origin = `DepRuntimeExport` なので intrinsic_keys から **必ず除外**。`PG_PASSWORD` は credential resolution で消費されたが target env には注入されない → consumer の env_allowlist 判定に出てこない → consumer の Pure/Closed 判定は credential rotation の影響を受けない
- teardown: uvicorn 停止 → postgres SIGTERM → `.ato-session` 削除

`PG_PASSWORD` rotation シナリオ:
1. host で `export PG_PASSWORD=new_value` → 再 `ato run`
2. lock は変わらない (template のまま、resolved value 不在)
3. instance_hash 不変 → 同 state.dir → 同 DB cluster
4. `.initialized` sentinel が存在 → provision はスキップ (= initdb 再実行で新 password を焼かない、これは provider 側で `ALTER USER` 等の rotation hook を別途定義する必要)
5. target start で resolve された credential が runtime_exports.DATABASE_URL に展開される → uvicorn が新 password で接続

ステップ 4 の rotation hook 設計は本 RFC scope 外。`[contracts.service@1]` に rotation 用 sub-command 概念を足すのは follow-up RFC で扱う (`CAPSULE_DEPENDENCY_CREDENTIAL_ROTATION`)。v1 では「credential を変えても state path は壊れない」までを保証する。

## 12. Follow-Up RFCs

本 RFC v1 と独立に決める必要があるもの:

- **CAPSULE_DEPENDENCY_TRANSITIVE_IDENTITY** — transitive deps を `dependency_derivation_hash` に再帰畳み込みする規則。Phase 2 cross-host reconstruct と一緒に。
- **CAPSULE_TOOL_CONTRACT** — `tool@1` の invoke / args / stdin / stdout / exit code 規約。
- **CAPSULE_DEPENDENCY_SHARED_STATE** — `ownership = "shared"` の scope と GC、複数 parent からの concurrent 書き込み調停。
- **CAPSULE_DEPENDENCY_VERSION_RESOLUTION** — npm 風 nested vs flat、major 衝突時の挙動 (v1 は禁止だがいずれ緩和したい場合)。
- **CAPSULE_DEPENDENCY_REFCOUNT** — 1 service instance を複数 consumer で共有する仕組み。`shared instance` semantics。
- **CAPSULE_DEPENDENCY_SUPPLY_CHAIN** — provider capsule の signature / SBOM / trust scope。
- **CAPSULE_DEPENDENCY_SECRET_IDENTITY** — secret parameter の hash-of-value による cross-host replay 同一性。
- **CAPSULE_DEPENDENCY_CREDENTIAL_ROTATION** — credential rotation を provider に通知する hook (`ALTER USER` 等を打ち直す sub-command 概念) の grammar。v1 では state path 不変までを保証、active session への credential 反映方法は別 RFC。
- **CAPSULE_DEPENDENCY_CREDENTIAL_DEFAULTS** — `credentials.<key>.default` を許す場合の安全条件 (literal 禁止 / secret-store 経由 / lint policy)。v1 で禁止としたのを将来緩和する場合の枠組み。
- **CAPSULE_DEPENDENCY_OBSERVABILITY** — `ato explain-hash` の dep 説明、`ato status` の dep lifecycle 表示、receipt の dep field expansion。

## 13. Open Questions (v1 内残課題)

本 RFC v1 を実装するまでに最低でも別 issue で詰める:

- **Local override**: `capsule://ato/postgres@16` を local path に差し替える grammar (`--with-dep db=path:./local-postgres-capsule` か config か manifest field)。lock identity の整合性 (override は lock を invalidate するか、lock 内に override marker を入れるか)。
- **テンプレ変数 namespace の最終 grammar**: `{{params.*}}`, `{{credentials.*}}`, `{{host}}`, `{{port}}`, `{{state.dir}}`, `{{deps.<name>.runtime_exports.*}}`, `{{deps.<name>.identity_exports.*}}`, `{{env.*}}` の 8 種を v1 で予約 (`{{socket}}` は v1.x で `unix_socket` runtime 追加時に予約)。escape (`\{\{...\}\}`?) を含む正式 grammar、評価順序、未定義 key の扱い。
- **`ready.timeout` の lock 出力**: timeout を lock 出力に書くか runtime のみか。書くと `dependency_derivation_hash` には含めない選択も含めて検討 (現状は runtime 値扱いで lock 入れない方針)。
- **Endpoint allocation の race**: `port = "auto"` で TCP socket を Ato が掴んでから provider に渡す間の race 回避。OS-assigned port を provider 自身に取らせて `<state.dir>/.port` に書かせる方式 vs Ato が握る方式の決定。`unix_socket = "auto"` 実装時にも同じ判断が必要。
- **Credential lifecycle in process memory**: resolved credential 値の保持期間 (resolve → 注入 → ゼロクリア) を Ato runtime が責任を持つか provider に任せるか。stdout/stderr 経由の漏洩防止 hook (`secret = true` の log filter) との整合。

## 14. Implementation Sketch (非規範)

仕様確定後の実装段取りメモ。RFC 本体には含めないが参考のため:

1. `capsule-core` に `[dependencies.*]` parser と lock 出力を実装 (既存 `manifest_v03.rs` 拡張)。`parameters` と `credentials` を別 block として parse、credentials は template form のまま AST に保持。
2. `unix_socket = "auto"` / `ready.type = "http" | "unix_socket"` は parser で AST に受理しつつ、lock phase で **fail-closed reject** (§9.1 verification 13)。
3. `capsule-core` に `[contracts.*]` parser を実装 (provider 側)。`parameters` と `credentials` を別 block として parse。`state.version` を必須 field として扱う (`state.required = true` の時)。
4. lock 時 contract verification (§9) を `routing/router.rs` 系で実装:
   - parameter / credential type & required check
   - credentials の literal 禁止 (`{{env.X}}` テンプレ必須)
   - **credentials の `default` field 宣言は parser/lock で reject** (§7.3.1 Hard invariant 7)
   - `{{env.<KEY>}}` の `<KEY>` が manifest top-level `required_env` に存在 (§5.2)
   - `identity_exports.<key>` 値文字列に `{{credentials.X}}` が出現したら lock 失敗 (§7.4)
   - `instance_hash` を `(resolved, contract, parameters)` から計算 (credentials は **入力に入れない**) して lock に書く
5. `ato-cli` の既存 `materialize_managed_services` / `orchestrate_managed_services` (内部実装名) を `dependencies` graph 駆動に切替。**外部仕様には `managed_service` 語彙を出さない**。
6. dynamic endpoint allocation (`port = "auto"` のみ、TCP) を runtime に追加。
7. ready probe runtime: v1 は `tcp` と `probe` のみ実装。
8. credential resolution: orchestration 直前に lock の template から `{{env.X}}` を resolve (host env から、scope = top-level required_env)、メモリ上保持、`{{credentials.X}}` を必要な場所で展開、終了時にゼロクリア。
9. **env capture model に origin tracking を追加** (§7.4.1)。`(key, value)` を `(key, value, EnvOrigin)` に拡張し、`DepRuntimeExport` origin の entry を `intrinsic_keys` 計算から除外する。
10. v2 receipt の `dependency_derivation_hash` に `(parameters, identity_exports)` を畳み込む (§9.5)。**credentials は決して入れない**。
11. teardown ordering と orphan **detection (warn-only)** を session lifecycle に組み込む (§10.4)。auto-kill / GC は別 RFC。
12. `runtime_exports.<key>.secret = true` の redaction を log writer / receipt builder / explain output に通す。
13. 検証 capsule として `ato/postgres` を 1 個書く。WasedaP2P を新 grammar に移行して E2E。
14. **Identity invariant test**: `PG_PASSWORD` を rotate して再 `ato run` した時に (a) lock が変わらない、(b) instance_hash が変わらない、(c) state.dir が同じ、(d) `dependency_derivation_hash` が変わらない、を検証する自動テストを 1 本入れる。これは v1.3 の最重要 invariant を守るレグレッション防止。

実装は `capsule-core` parser / lock → `ato-cli` runtime + env model 改修 + credential resolution → 検証 capsule の 3 層を順番に通す。

## 15. Implementation Plan

詳細な phase 分解、PR 順序、テスト戦略、リスクは:

→ **[`docs/plan_capsule_dependency_contracts_20260504.md`](../../plan_capsule_dependency_contracts_20260504.md)**

7 phase / 7 PR、推定 5–6 週。critical path は `parser → env origin → lock verification → orchestration → E2E`。credential 経路 (P4) は orchestration (P5) の前提として並行進行。
