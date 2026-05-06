# RFC: Capsule Calls Capability

**Status**: Draft
**Target**: docs/rfcs/draft/
**Layer**: Part I (Foundation spec) — manifest schema 拡張
**Created**: 2026-05-05
**Schema impact**: capsule.toml v0.3 → v0.4(後方互換、新規 optional セクション)

---

## 0. Summary

Capsule が runtime で他の capsule を呼び出せるようにする capability `[capsule_calls]` を manifest に追加する。許可は **caller capsule の manifest で declarative に宣言する allowlist** と、**初回呼び出し時の user consent modal** の二段構えで行う。

これは ato が Docker / 既存 container runtime と category として分かれる土台になる primitive である。"Share software like recipes" の比喩を、distribution(レシピを配る)から ecosystem(レシピが他のレシピを呼ぶ)へ拡張する。

---

## 1. Motivation

### 1.1 既存 capability では表現できない操作

現行 manifest spec(v0.3)は以下の capability を持つ:

- `[network]` egress allowlist
- `[isolation]` 環境変数 allowlist
- `[state.workspace]` filesystem 境界
- host bridge による IPC

しかし「capsule A が runtime で別の capsule B を起動・操作する」という操作の capability は未定義。現状は実装可能性が ad-hoc に存在するだけで、permission も attribution も走らない。

### 1.2 capsule-in-capsule が成立する構造的優位

ato は historical に「アプリが他のアプリを呼ぶ」設計が失敗してきた領域(OLE/COM、Android Intents、AppleScript、Web Intents)を、3つの構造で回避できる:

1. **Declarative**: caller の manifest で allow list が事前に見える(install 時 / `ato run` 開始時に user / 監査者が確認可能)
2. **Same trust plane**: callee の capability は caller に grant された capability の subset でしかありえない、という制約を spec で書ける
3. **Immutable identity**: callee は `capsule://` の point-in-time identity で指される(§1.2 mutable reference 禁止が既に効く)

この3点が揃うのは新規であり、本 RFC はそれを capability として spec 化する。

### 1.3 戦略的位置づけ

本 capability は ato Layer 2 (Protocol moat) の中核。Phase 1 では発信しないが、Phase 1.5 で標準 renderer 系 capsule(dbml-renderer 等)が最初の使い手になる。Phase 2(Self-hackathon)で user-facing として打ち出す。

---

## 2. Goals / Non-goals

### Goals

- caller capsule の manifest で「呼び出しうる callee の集合」を declarative に宣言できる
- runtime で実際に呼び出す瞬間に user consent を介在させる
- ato の他 capability(`network`, `isolation`)と **完全に対称な** spec 形式を持つ
- npm 級の依存解決機構を **導入しない**(call は依存ではなく権限)
- §1.2 の mutable reference 禁止 rigor を allowlist にも適用する

### Non-goals

- callee 側 policy(誰から呼ばれることを許すか)── 別 RFC(`CALLER_POLICY`)で扱う
- version range の解決機構 ── 別 RFC で扱う
- "spawn child window" / "pipe to capsule" / "daemon capsule" 等の より強い call 形式 ── 本 RFC は `embed` / `api` interface call のみを対象とする
- Foundation の Schema Registry / capsule registry の検索 ── 本 RFC のスコープ外

---

## 3. Design principles

| 原則 | 本 RFC での適用 |
|---|---|
| One boundary, one policy | 既存 capability と同形式の allowlist |
| Safe by default | `[capsule_calls]` 未宣言なら他 capsule を呼べない |
| Declare first, materialize later | manifest 宣言 → runtime grant → 実行 |
| Mutable references are banned | allowlist に floating alias 禁止 |
| Reuse the model, not special cases | 新 primitive を増やさない、既存 capability 形式の拡張 |

---

## 4. Specification (Part I, normative)

### 4.1 Manifest schema

```toml
schema_version = "0.4"

[capsule_calls]
allow = [
  "capsule://ato.run/std/dbml-renderer",
  "capsule://ato.run/std/svg-renderer@1.2.3",
]
allowed_interfaces = ["embed", "api"]
```

#### `[capsule_calls]` セクション

- **省略時の意味**: caller capsule は他のいかなる capsule も呼び出せない(deny-by-default)
- **存在する場合**: `allow` および `allowed_interfaces` を解釈する

#### `allow` フィールド

型: `string[]`

各要素は以下のいずれかの形式:

| 形式 | 例 | 意味 |
|---|---|---|
| Exact identity | `capsule://ato.run/std/dbml-renderer@1.2.3` | 特定の immutable revision のみ |
| Name without version | `capsule://ato.run/std/dbml-renderer` | 同名 capsule の任意 version(将来 version range で精緻化可能) |
| Wildcard all | `["*"]` | 任意の capsule を呼び出し可能(special form) |

#### 禁止される形式(MUST 拒否)

`allow` の各要素は §1.2 と同じ rigor を満たさなければならない。以下は validation error:

- Floating alias: `@latest`, `@stable`, `@nightly`
- Range operator: `@^1.2`, `@~1.2`, `@>=1.2`(将来 RFC で版規定までは禁止)
- Wildcards in path segment: `capsule://ato.run/std/*`, `capsule://ato.run/*/foo`
- Authority-level wildcard: `capsule://ato.run/`(末尾 `/` だけ)
- Mutable git refs: `@main`, `@HEAD`

`["*"]` のみが特例として全許可を意味する。これは development / power user 向けであり、Store publish 時に warning を発すること(SHOULD)。

#### `allowed_interfaces` フィールド

型: `string[]`

呼び出せる callee の interface 種別を限定する。本 RFC で受理する値:

| 値 | 意味 |
|---|---|
| `embed` | callee の `interfaces.embed`(inline render surface)を呼べる |
| `api` | callee の `interfaces.api`(HTTP/JSON-RPC machine API)を呼べる |

省略時の default: `["embed", "api"]`。

将来 RFC で追加されうる値(本 RFC では拒否):

- `ui` ── child-window/tab spawn を要する full UI surface
- `cli` ── サブプロセスとしての CLI 実行
- `mcp` ── agent-facing MCP server への接続
- `worker` / `daemon` ── 長寿命プロセス

これらは権限重みが異なるため、別 capability として独立に議論する。

### 4.2 allow と既存 trust model の関係

allowlist に書かれただけでは callee は起動されない。実際の起動は §5 の runtime consent を必須とする。allow は **「caller がその capsule を呼ぼうとしうる」という宣言** であり、**「user が許可した」ことを意味しない**。

### 4.3 allow の subset 性

callee に grant される capability は、caller に grant された capability の **subset** でなければならない(MUST)。具体的には:

- caller が `network.egress` を持たないなら、callee も network egress を持てない
- caller が host bridge `ollama` を持たないなら、callee に転送できない

これにより capability の権限拡大攻撃(privilege escalation via call)を構造的に防ぐ。詳細な subset 計算規則は別 RFC `CAPABILITY_SUBSET` で規定する(本 RFC は subset 性のみ規定し、計算規則は deferred)。

---

## 5. Runtime semantics (Part II, ato 参照実装)

本セクションは Foundation spec ではなく ato 参照実装の挙動を規定する。他 conforming runtime は同等の安全性を達成する別実装を取りうる。

### 5.1 Pre-launch disclosure

`ato run <caller>` 起動時、ato は caller manifest の `[capsule_calls]` を user に表示する:

```
Capsule "my-notes" may call the following capsules:
  • capsule://ato.run/std/dbml-renderer (any version)
  • capsule://ato.run/std/svg-renderer@1.2.3

Allowed interfaces: embed, api

[Continue] [Cancel]
```

`["*"]` の場合は強い警告を伴う表示にする(SHOULD)。

### 5.2 First-call consent modal

caller が runtime で初めて特定の callee を呼ぼうとしたとき、modal を表示する:

```
my-notes wants to call:
  capsule://ato.run/std/dbml-renderer@1.2.0
  via interface: embed

[Allow once]  [Allow for session]  [Allow always]  [Deny]
```

### 5.3 Grant scope

| Scope | 永続化先 | 範囲 |
|---|---|---|
| `once` | なし | この1回の call のみ |
| `session` | in-memory | この `ato run` session が終了するまで |
| `always` | `~/.ato/grants.json` | user が revoke するまで永続 |
| `deny` | session in-memory(SHOULD) | 同 session 内で再度 modal を出さない |

`always` の grant は ato Desktop の Permissions 画面で一覧・revoke できる(MUST)。

### 5.4 Modal を出さないケース

以下では modal を出さない:

- 同 session 内で同じ (caller, callee@version, interface) の組み合わせが既に grant されている
- user が `--allow capsule_calls=...` を CLI で明示指定している(test / CI 用)
- caller の allowlist に callee が含まれていない場合は modal すら出さず即座に **denied**(consent fatigue 回避)

### 5.5 Pre-grant の禁止

ato 参照実装は以下を行わない:

- caller の manifest 宣言を理由に、user 確認なしに grant する
- 同一 publisher の capsule に対する暗黙 trust(verified publisher 制度は将来 RFC)
- `["*"]` を理由に、user 確認なしに任意の callee を起動する

`["*"]` であっても、初回呼び出し時に modal を出す(MUST)。

---

## 6. Trust model boundaries

### 6.1 本 RFC が扱う方向

```
caller capsule
  ↓ "B を呼ぶ権限を持つか"
ato runtime (allow + consent)
  ↓ if granted
callee capsule
```

### 6.2 本 RFC が扱わない方向(deferred)

```
callee capsule
  ↓ "誰から呼ばれることを許すか"
caller_policy (future RFC)
```

callee 側で「特定の caller のみ受け入れる」「user-present な call のみ受ける」等の policy を書きたいケースは将来発生する。たとえば:

- secret を持つ callee が、任意の caller から呼ばれて secret を間接使用されないよう制限
- rate-limited な API quota を持つ callee が、quota 消費を caller 経由で爆発させない
- local-first / privacy-sensitive な callee が、UI なしの自動 call を拒否

これらは別 RFC `CALLER_POLICY` で扱う。本 RFC では callee は誰から呼ばれても同じ機能を提供する前提とする。

### 6.3 Capability privilege escalation 防止

§4.3 の subset 性により、A が B を呼んでも B の権限は A の subset に閉じる。これにより:

- 弱権限 capsule が強権限 capsule を呼んで権限拡大することを構造的に阻止
- audit 時に「なぜ B が network に出たか」を「A が grant されていたから」で追跡可能

---

## 7. Compatibility & migration

### 7.1 Schema version bump

本 RFC は capsule.toml schema を v0.3 → v0.4 に上げる。`[capsule_calls]` セクションは optional であり、未宣言の v0.3 capsule は v0.4 runtime 上で deny-by-default として動作する(他 capsule を一切呼べない)。

### 7.2 既存 capsule への影響

v0.3 capsule の manifest を変更する必要はない。capsule call が必要な capsule は以下を行う:

1. `schema_version = "0.4"` に更新
2. `[capsule_calls]` セクションを追加
3. 必要なら `allowed_interfaces` を限定

### 7.3 Validation

ato CLI / Store は `[capsule_calls]` セクションについて以下を validate する(MUST):

- 各 allow 要素が §4.1 の grammar に合致する
- floating alias / range / wildcard path segment を含まない
- `allowed_interfaces` の各値が本 RFC で受理されている値である
- `["*"]` を Store publish しようとした場合 warning を発する(SHOULD)

---

## 8. Foundation vs ato 実装の境界

| 要素 | Layer | 理由 |
|---|---|---|
| `[capsule_calls]` セクションの存在と grammar | Part I | 他 conforming runtime も解釈する必要がある |
| `allow` / `allowed_interfaces` の semantics | Part I | 同上 |
| Subset capability rule | Part I | security 不変条件のため |
| Pre-launch disclosure UI | Part II | 各 runtime が独自に実装してよい |
| Consent modal の文言・選択肢 | Part II | 同上 |
| `~/.ato/grants.json` の format | Part II | ato 参照実装の永続化詳細 |
| `--allow` CLI flag | Part II | ato CLI の便宜 |

---

## 9. Open questions

以下は本 RFC では決定しない:

- **Q1**: `allow` の version range syntax(`@^1.2` 等)を将来サポートする場合、どの記法か(SemVer 互換 / npm 互換 / 独自)
- **Q2**: 同一 (caller, callee) の組み合わせで callee の version が異なる場合、grant は version 単位か name 単位か
- **Q3**: callee がさらに別 callee を呼ぶ場合(transitive call)、user consent はどう積み重なるか(consent fatigue 対策)
- **Q4**: `["*"]` を持つ capsule は untrusted publisher として扱うべきか、それとも単なる convenience として扱うか
- **Q5**: ato Desktop の Permissions 画面で grant を revoke した場合、現在 running の callee はどう扱うか(graceful shutdown / immediate kill / 次回起動時から有効)

これらは本 RFC accepted 後の独立 issue で議論する。

---

## 10. Future work

本 RFC が accept された後、以下の RFC を順次起草する:

- `CALLER_POLICY` — callee 側で caller を制限する仕組み
- `CAPABILITY_SUBSET` — caller → callee の capability 継承計算規則の精密化
- `INTERFACE_TYPES_EXTENDED` — `ui`, `cli`, `mcp`, `worker` interface の追加と call 規則
- `VERIFIED_PUBLISHER` — publisher 単位の trust tier と pre-grant
- `TRANSITIVE_CONSENT` — A → B → C の consent 連鎖モデル

---

## 11. References

- idealspec.pdf §1.2 — `@version-id` point-in-time identity 規則
- idealspec.pdf §2 — capsule.toml v0.3 spec
- idealspec.pdf §0 — Copy vs Imitation コアテーゼ
- idealspec.pdf §12 — 設計原則(Safe by default, One boundary one policy 等)
- pitch.md セクション11 — 将来の弾候補(本 RFC は Phase 2 主砲の spec 基盤)
- ato_yc_strategy_v0.1.md §1 Layer 2 — Protocol moat の中核要素
- 議論ログ(2026-05-05 conversation)— allowlist + runtime modal 設計の起点

---

## 12. Revision history

- v0.1 (2026-05-05): 初版 draft。allowlist + consent modal の二段構え、subset capability、Part I / Part II 分離、`["*"]` を special form として規定。
