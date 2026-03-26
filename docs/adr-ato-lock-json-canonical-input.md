# ADR: ato.lock.json As Canonical Input

- Status: Accepted
- Date: 2026-03-25
- Decision Makers: ato-cli maintainers
- Related: [current-spec.md](/Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-cli/docs/current-spec.md), [architecture-overview.md](/Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-cli/docs/architecture-overview.md), [bc-implementation-design.md](/Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-cli/docs/bc-implementation-design.md)

## 1. Context

ato-cli の現行実装は manifest-first であり、`capsule.toml` が authored source of truth、`capsule.lock.json` が manifest 補助ロック、`config.json` が Nacelle 向け派生 IR という三層で成立している。

この構造は初期の機能追加には有効だったが、次の問題を抱えている。

- 実行契約、再現性、環境依存割り当て、実行時承認が別々のファイルとコード経路に分散している
- `capsule.lock.json` が再現性コアではなく manifest 従属の補助ロックに留まっている
- `config.json` や execution plan の導出元が manifest 中心で、lock-first な検証や署名境界を作りにくい
- policy / consent / provisioning / runtime guard の情報が execution path ごとに分かれており、1 つの canonical input にまとまっていない
- 将来的な CAS / closure / remote cache / target-specific reproducibility を考えると、manifest-first のままでは責務分離が弱い

同時に、現行の fail-closed 実装、execution plan、runtime guard、`config.json` 生成はすでに有用な内部モデルを持っており、全てを捨てて再設計する必要はない。

この ADR は、toml レスかつ lock オンリーなアーキテクチャへ移行するための canonical input と移行原則を定義する。

## 2. Decision

ato-cli は、新しい canonical input として `ato.lock.json` を導入する。

`ato.lock.json` が存在する場合、それは唯一の authoritative execution input とする。`capsule.toml` と既存 `capsule.lock.json` は、その場合の実行意味論を上書きまたは変更してはならない。

`ato.lock.json` は、従来の `capsule.toml` と `capsule.lock.json` が担っていた責務を再編し、次の 5 つの論理層を持つ。

1. `resolution`
2. `contract`
3. `binding`
4. `policy`
5. `attestations`

このうち、再現性と署名のコアは `resolution` と `contract` を中心に構成する。`binding` と `attestations` は mutable かつ environment-specific なスコープとして論理分離する。

`ato.lock.json` は唯一の canonical input とするが、移行期間中は `capsule.toml` と既存 `capsule.lock.json` を compatibility input として受け入れる。

本 ADR で固定する規範事項は次のとおりである。

- `ato.lock.json` は canonical input として扱わなければならない
- execution plan と `config.json` は canonical input ではなく派生物として扱わなければならない
- `binding` と `attestations` は `lock_id` に影響してはならない
- `policy` は論理上の execution model に属するが、canonical reproducibility projection には含めてはならない
- `capsule.toml` と既存 `capsule.lock.json` は、`ato.lock.json` が存在する場合 canonical input として扱ってはならない

本 ADR では次を意図的に後続設計へ送る。

- `binding` と `attestations` の最終的な保存媒体
- signature block の最終フォーマット
- registry API の最終 wire format

## 3. Why This Decision

この決定の理由は次のとおりである。

- manifest-first から lock-first へ責務の中心を移すことで、再現性、署名、検証、execution planning を 1 つの入力に寄せられる
- `config.json` を廃止対象ではなく派生 IR と位置づけ直すことで、Nacelle 連携を壊さずに canonical input だけを切り替えられる
- `binding` と `attestations` を分離することで、repo-tracked な再現性コアと、host-local な割り当てや承認結果を混同しないで済む
- dual-path を前提にすることで、既存の manifest-first プロジェクトを即時破壊せずに段階移行できる
- execution plan と runtime guard の既存資産を、lock-first 設計の中間 IR として流用できる

## 4. Canonical File Name And Compatibility

### 4.1 Canonical file name

新しい canonical file name は次とする。

- `ato.lock.json`

### 4.2 Existing lockfile relation

既存の `capsule.lock.json` は manifest 補助ロックであり、本 ADR が定義する lock-first スキーマとは責務が異なる。

したがって、既存 `capsule.lock.json` を単純拡張して canonical input に昇格させない。`ato.lock.json` は別系統の新規スキーマとして扱う。

### 4.3 Compatibility inputs

移行期間中、次を compatibility input として受け入れる。

- `capsule.toml`
- 既存 `capsule.lock.json`

これらは canonical source ではなく、`ato.lock.json` を生成する import source とする。

`ato.lock.json` と compatibility input が同時に存在する場合、`ato.lock.json` を authoritative とする。compatibility input との差分は diagnostics として報告してよいが、自動再 import や意味論のマージは明示 opt-in なしに行ってはならない。

### 4.4 Schema version policy

新しい canonical file である `ato.lock.json` の初期 on-disk schema version は `1` とする。

過去の議論で使われた `v3` はアーキテクチャ草案の世代番号であり、`ato.lock.json` のファイル形式の継承番号ではない。

## 5. Format And Serialization Rules

`ato.lock.json` は canonical JSON とする。

- 安定ハッシュには JSON Canonicalization Scheme 相当を用いる
- 既存の `serde_jcs` 実装資産を活用できるようにする
- machine-generated を前提とする
- 人手編集は可能だが推奨しない
- 利用者向けの編集支援は `preview`, `inspect`, `diagnostics`, `remediation` で担保する

`generated_at` のような informational metadata は canonical hash と signature payload の対象外とする。これらは再現性コアではなく、生成時の補助情報として扱う。

`lock_id` の self-consistency 検証は、`lock_id` field 自身および `signatures` を含まない canonical projection に対して再計算して行う。

未対応 feature の silent ignore は認めない。

- strict mode では validation error
- non-strict mode でも security / identity / reserved env namespace / readonly-rootfs などは fail-closed を優先する

## 6. Hash And Signature Boundary

### 6.1 Canonical hash scope

最低限、次を canonical hash 対象に含める。

- `schema_version`
- `resolution`
- `contract`

`lock_id` は、上記の canonicalized projection から計算される。algorithm prefix は必須であり、少なくとも `blake3:` 形式をサポートする。

canonicalization 規則または projection 規則が将来変更された場合、意味内容が同一でも `lock_id` が変化しうることを許容する。

### 6.2 Mutable or local scope

次は canonical hash から分離する。

- `generated_at`
- `binding`
- `policy`
- `attestations`
- `signatures`
- `observations`
- host-local grants
- diagnostic-only metadata

### 6.3 Lock identity

`lock_id` は canonicalized projection から生成される lock 識別子とする。`binding`、`attestations`、informational metadata、host-local state は `lock_id` に影響してはならない。

`policy` の差異はホストごとの execution permission を変えうるが、`lock_id` を変えてはならない。

例:

```json
"lock_id": "blake3:..."
```

### 6.4 Signature scope

標準署名は、`lock_id` を計算した canonical projection に対して結び付かなければならない。

原則:

- 標準署名の検証対象は `schema_version + resolution + contract` を基礎とする canonical projection である
- `binding`、`policy`、`attestations`、informational metadata を標準署名対象へ暗黙に拡張してはならない
- non-canonical または host-local scope に対する追加署名を実装してもよいが、それらは型付きで明示されなければならず、canonical lock identity を再定義してはならない

## 7. Top-Level Model

`ato.lock.json` は次のトップレベル構造を持つ。

```json
{
  "schema_version": 1,
  "lock_id": "blake3:...",
  "generated_at": "2026-03-25T00:00:00Z",
  "features": {
    "declared": [],
    "required_for_execution": [],
    "implementation_phase": {}
  },
  "resolution": {},
  "contract": {},
  "binding": {},
  "policy": {},
  "attestations": {},
  "signatures": []
}
```

論理上は最低限、次を分離する。

- `resolution`
- `contract`
- `binding`
- `policy`
- `attestations`

downstream consumer は、永続化の有無にかかわらず lock-shaped な内部モデルを入力として扱わなければならない。persisted `ato.lock.json` は、そのモデルの canonical projection と許可された logical section を durable に直列化した表現である。

## 8. Section Semantics

### 8.1 `features`

`features` は lock が使用している capability 群と、実装フェーズ依存の validation を支える。

- `declared`: lock が使用している feature 名一覧
- `required_for_execution`: 未対応なら実行不能な feature
- `implementation_phase`: 段階導入を補助する diagnostic metadata。存在しても execution semantics や `lock_id` に影響してはならない

追加規範:

- parser acceptance は `implementation_phase` の有無や値に依存してはならない
- validation severity を `implementation_phase` によって緩和してはならない
- execution planning は `implementation_phase` を必須入力としてはならない

### 8.2 `resolution`

`resolution` は再現性の核であり、execution plan を導出する上位入力である。

責務:

- source の固定
- runtime / toolchain / closure の固定
- target ごとの解決結果
- launcher の固定
- contract 関連 digest の参照

最低限のルール:

- persisted `ato.lock.json` は partially resolved であってよいが、その場合 unresolved state は schema 上の first-class marker として明示されなければならない
- unresolved marker は reason-bearing であるべきであり、少なくとも insufficient evidence、ambiguity、deferred host-local binding、policy-gated resolution、explicit selection required のような理由クラスを保持できなければならない
- `resolved_targets` は 0 件であってよい。ただしその場合、target selection または target compatibility の unresolved marker が必要である
- execute / install / publish など target-usable state を要求する flow では、互換 target が少なくとも 1 件解決されるまで fail-closed とする
- `closure_digest` は package dependency 単体ではなく実行 closure 全体を表す
- `locators` は retrieval hint であり identity ではない
- target 不一致時は fail-closed

### 8.3 `contract`

`contract` は workload の portable な実行契約を表す。

主要セクション:

- `process`
- `workloads`
- `identity`
- `compute`
- `network`
- `filesystem`
- `storage`
- `secrets`
- `security`
- `supervisor`
- `metadata`
- `config_projection`
- `lifecycle`
- `env_contract`

ここで定義された内容は authored intent に相当し、host-specific な具体値を直接持たないことを原則とする。

`contract` には runtime-independent な要求だけを置く。runtime materialization detail は派生 IR または host-local binding に閉じ込める。

特に `supervisor`、`metadata`、`config_projection` は transport や mount 実装を定義する場所ではなく、capability 要求または projection 要求を表す。

### 8.4 `binding`

`binding` は environment-specific な割り当てを表す。

例:

- host 固有 path
- host port へのマッピング
- identity provider の選択
- supervisor transport と endpoint
- injected env の具体値

`binding` は repo-tracked に含めてもよいが、論理的には mutable かつ regenerable とみなす。

既定の運用推奨として、`binding` は repo-tracked canonical lock content の外側に保持する。canonical schema 上に field は存在してよいが、既定では workspace-local または sidecar state を優先し、repo への write-back は opt-in とする。

`binding` は logical section であり、embedded してもよい。しかし default tooling は repo-tracked binding content を自動生成してはならず、workspace-local binding state を優先しなければならない。

binding source の既定 precedence は次のとおりとする。

1. explicit CLI input
2. workspace-local binding state
3. embedded `binding`
4. unresolved or runtime default

publish / export / attestable artifact では embedded `binding` を既定で除外し、含める場合は明示 opt-in とする。

### 8.5 `policy`

`policy` は host / organization / workspace が許容する enforcement guardrail を表す。

portable かつ artifact に随伴すべき publisher-declared constraints は `contract` に属する。この ADR における `policy` は host-local enforcement policy を主に指す。

`policy` は logical execution model の一部だが、canonical reproducibility projection の一部ではない。実装は `policy` を embedded lock content、workspace-local state、organization-local policy bundle のいずれから取得してもよい。

原則:

- deny が allow より優先
- contract が policy を超えたら fail
- policy の差異は execution 可否を変えてよいが、`lock_id` を変えてはならない

具体的な保存形式が 1 ファイル内であるか sidecar であるかは後続設計に送るが、意味論としては `contract` と区別する。

### 8.6 `attestations`

`attestations` は承認、観測、学習結果の記録である。

原則:

- canonical hash に含めない
- repo diff ノイズを避けるため workspace-local 分離を推奨する

既定の運用推奨として、`attestations` は repo-tracked canonical lock content の外側に保持する。repo へ埋め込む運用は opt-in とする。

## 9. Derived Artifacts

`ato.lock.json` は唯一の canonical input だが、内部派生物は残す。

派生物として残すもの:

- execution plan
- `config.json`
- runtime-specific launch spec

特に `config.json` は廃止対象ではなく、Nacelle 向けの内部 IR として残す。

位置づけは次のとおりとする。

- canonical input ではない
- `ato.lock.json` から生成される
- runtime materialization のための内部中間表現である

execution plan と `config.json` は `ato.lock.json` と、必要であれば明示的に選択された local binding / policy input を元に決定的に再生成可能でなければならない。許可されていない ambient host state を暗黙入力として参照してはならない。

## 10. Dual-Path Migration Contract

manifest-first から lock-first へは一気に切り替えない。移行期間中は dual-path を前提とする。

推奨解決順序:

1. `ato.lock.json` があれば読む
2. なければ `capsule.toml` を import して一時 lock を生成する
3. どちらもなければ bootstrap flow に入る

`ato.lock.json` が存在する場合、`capsule.toml` と既存 `capsule.lock.json` は diagnostics や provenance 表示のために参照してよいが、実行入力として解釈してはならない。

この移行を成立させるため、少なくとも次の互換コンパイラが必要である。

- manifest -> lock compiler
- execution plan from lock
- config from lock

## 11. Diagnostics And Remediation

移行後の diagnostics は manifest path ではなく lock path を基準に返す。

例:

- `contract.network.egress[0].host` is not approved
- `contract.process.entrypoint` is unresolved

これにより、canonical input が `ato.lock.json` に移った後も、利用者が修正箇所を一意に特定できる。

dual-path 移行中は、可能な場合 diagnostics は lock path を主としつつ、import source への provenance または source mapping を補助的に添えてよい。

## 12. Strict Mode

strict mode では次を error とする。

- `features.required_for_execution` に未対応項目がある
- required capability が未実装である
- security-sensitive capability が enforcement 不能である

特に次は warning ではなく fail を優先する。

- `security.read_only_root_fs`
- `identity.*`
- `env_contract.reserved_prefixes`
- required supervisor capability

## 13. Implementation Phases

### Phase 0

- ADR 確定
- JSON Schema 定義
- serde 型定義
- canonical serialization
- feature validation

### Phase 1

- manifest -> lock compiler
- execution plan from lock
- config from lock
- run dual-path
- `process`, `filesystem`, reserved env namespace, readonly-rootfs, basic lifecycle の実装

### Phase 2

- workloads role separation
- runtime / workload identity split
- policy enforcement の強化

### Phase 3

- config projection
- metadata plane
- supervisor IPC
- ephemeral storage quota enforcement

### Phase 4+

- build / package / install / publish の lock-first 化
- registry key を `manifest_hash` から `lock_id` / `closure_digest` へ移行
- preview / diagnostics / init の再設計
- authored source としての `capsule.toml` 廃止

## 14. Consequences

### Positive consequences

- reproducibility, execution contract, policy, approval を 1 つの canonical model に統合できる
- execution plan と runtime guard を lock-first に再編しやすくなる
- `config.json` を派生 IR として維持できるため、Nacelle との境界を壊さない
- CAS / closure / target-specific reproducibility / remote cache へ自然に拡張できる

### Costs and trade-offs

- 現行の manifest-first 実装と artifact layout の互換層をしばらく維持する必要がある
- build / install / publish / registry metadata の key を再編するコストが大きい
- diagnostics, preview, init, remediation の UX を lock path ベースへ作り直す必要がある
- `binding` と `attestations` の保存場所を repo, workspace, host local のどこに置くかを明確に運用設計する必要がある

### Explicit non-decisions

この ADR は次をまだ確定しない。

- `binding` と `attestations` を 1 ファイル内に常駐させるか、workspace-local ファイルへ分離するかという最終的保存戦略
- signature block の最終フォーマット
- registry API の最終 wire format

ただし、未決なのは最終的保存媒体であって、既定の運用推奨は決まっている。`binding` と `attestations` は、既定では repo-tracked canonical lock content の外側に保持する。

これらは本 ADR の決定に従う後続 ADR または implementation design で確定する。

## 15. Canonical Summary

本 ADR の最終要点は次のとおりである。

1. `ato.lock.json` を canonical input とする
2. 既存 `capsule.lock.json` とは別系統の新規スキーマとする
3. `config.json` は廃止せず、Nacelle 向け派生 IR として残す
4. `binding` / `policy` / `attestations` は論理モデルに含めつつ再現性コアから分離する
5. partially resolved durable lock を明示 marker 付きで許容し、実行可能性は downstream flow で fail-closed に判定する
6. dual-path により manifest-first から lock-first へ段階移行する

この方針により、ato-cli は manifest 補助ロック中心の設計から、lock-first かつ fail-closed な canonical runtime model へ移行する。