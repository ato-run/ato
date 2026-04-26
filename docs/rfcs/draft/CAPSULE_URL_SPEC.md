---
title: "Capsule URL Spec"
status: draft
date: "2026-04-21"
author: "@koh0920"
ssot:
  - "apps/ato-cli/core/src/handle.rs"
related:
  - "docs/rfcs/accepted/CAPSULE_HANDLE_SPEC.md"
  - "docs/rfcs/accepted/SIGNATURE_SPEC.md"
  - "docs/rfcs/accepted/IDENTITY_SPEC.md"
---

# Capsule URL Spec

> Status: **Draft**. This document defines the **Layer 1 grammar** of the
> `capsule://` URL scheme as an identity protocol. Authority-specific path
> semantics live in separate documents (e.g. `CAPSULE_HANDLE_SPEC.md` for
> `ato.run`).

## 1. Scope

本書は `capsule://` URL の grammar、identity semantics、resolution model
を定義する。Path segment の意味、予約語、version 文法など **authority ごとに
異なる解釈** は本書の範囲外とし、各 authority が別文書（"authority policy"）
として定める。

この分層は以下の原則に基づく:

- **URL は identity である**。同じ URL は任意の時点で高々1つの capsule を指す。
- **Grammar は普遍、semantics は authority 固有**。scheme レベルで path
  structure を固定すると将来の namespace モデル（3階層、scoped package、
  git ref 等）を受容できない。
- **Identity resolution は URL 単体で閉じる**。runtime fetch される
  configuration に依存する identity 解釈は認めない（§3 参照）。

## 2. URL Grammar (Layer 1)

### 2.1 ABNF

```
capsule-url     = "capsule://" authority "/" path [ "@" version-id ]

authority       = host [ ":" port ]
host            = <valid host per RFC 3986 §3.2.2>
port            = 1*DIGIT

path            = segment *( "/" segment )
segment         = 1*unreserved-segment
unreserved-segment = <any character except "/" "@" WSP CTL>

version-id      = 1*unreserved-version
unreserved-version = <any character except "/" "@" WSP CTL>
```

### 2.2 Normative Constraints

- `authority` は RFC 3986 の host (+ optional port) と解釈する。
- `path` は **1個以上の** segment を持たねばならない。segment 数の上限・
  下限は本 spec では定めない（authority policy が定める）。
- 各 `segment` は非空であり、`/`, `@`, 空白、制御文字を含んではならない。
- `version-id` は非空であり、`/`, `@`, 空白、制御文字を含んではならない。
- scheme, authority は case-insensitive（RFC 3986 §3.1, §3.2.2 準拠）。
- `path` および `version-id` は **default で case-sensitive** とする。
  Authority policy は特定の path segment について `case_sensitive = false`
  を宣言してよい。ただし `version-id` の case sensitivity を authority が
  変更することは禁止する（signing hash の stability を保つため）。

### 2.3 Normalization

Parser は parse 時に以下を **この順序で** 適用する:

1. Percent-encoding を RFC 3986 §6.2.2 に従い正規化する
   （先頭で行うことで `%40` 等が後続の `@`/`/` 判定を狂わせない）
2. scheme と authority を小文字化する
3. **Authority alias の正規化**: 以下は deprecated alias として受理し、
   canonical authority に変換する:
   - `capsule://store/...` → `capsule://ato.run/...`
4. Trailing `/` を削除する

この順序は normative であり、実装間で挙動差を生まないために従わねば
ならない（例: `Capsule://STORE/acme/app/` のような入力は手順 2 で
authority `store` となり、手順 3 で `ato.run` に正規化される）。

## 3. Identity Semantics

### 3.1 Point-in-Time Identity

capsule URL は **point-in-time identity** を表す。形式的には:

> **Invariant**: ある時刻 `t` において、capsule URL `U` を resolution した
> 結果は高々1つの capsule artifact `A(t)` を指す。`A(t)` が存在する場合、
> それは **同じ時刻においてどの runtime / tooling が resolution しても
> 同じ content hash を持つ** artifact である。

この invariant は signing, attestation, reproducibility の基盤である。

### 3.2 Mutable Reference の禁止

`version-id` は **immutable reference** でなければならない。以下は禁止:

- Range operator (`^1.2`, `~1.2`, `>=1.2`)
- Wildcards (`*`, `1.2.*`)
- Floating alias (`latest`, `stable`, `nightly`)
- Mutable git refs (branches, moving tags)

**例**:
- ✅ `capsule://ato.run/acme/app@1.2.3` — exact semver (immutable)
- ✅ `capsule://github.com/acme/app@a1b2c3d4e5f6...` — git commit SHA (immutable)
- ❌ `capsule://ato.run/acme/app@latest` — floating alias
- ❌ `capsule://github.com/acme/app@main` — mutable branch
- ❌ `capsule://ato.run/acme/app@^1.0` — range operator

`@version-id` を省略した場合の解釈は authority policy が定める（典型的には
「最新の安定リリース」だが、これは resolution 時の解釈であって URL の
identity には含まれない）。

本 spec は mutable reference の禁止を **semantic 要件** として規定する。
「どの文字列が mutable か」の **syntactic な判定は authority policy の
`version_id` 文法（典型的には regex pattern）に委ねる** 。これにより、
`latest` が ato.run では reject され（semver pattern にマッチしないため）、
GitHub 系 authority では git ref として扱われるが mutable と判明した場合に
reject される、といった authority 固有の判断が可能になる。

### 3.3 Path Semantics is Authority-Defined

path segment の意味（publisher, slug, scope, team, org, etc.）は
**authority ごとに異なる**。Runtime および tooling は、scheme 単独では
segment の意味を仮定してはならない。

例:

- `ato.run` は 2-segment: `publisher/slug` （`CAPSULE_HANDLE_SPEC.md` 参照）
- `github.com` は 2-segment: `owner/repo`
- 将来の `corp.internal` は 3-segment を採用してよい: `org/team/project`

Authority policy を知らない tooling は "path segment" としてのみ扱い、
segment 意味に依存するエラーメッセージ・UI 表示を行ってはならない。

## 4. Authority Policy

### 4.1 Definition

**Authority policy** は `authority` ごとの path semantics を定義する静的文書。
内容:

- path segment 数とそれぞれの意味
- 予約語（publisher 名、slug prefix 等）
- `version-id` の文法（semver / git SHA / date-based 等）
- 各 segment の case sensitivity
- 各 segment の長さ・文字制約（grammar を超える authority 固有制約）

### 4.2 Distribution

Authority policy は **spec-level document** として扱い、**runtime resource
として扱わない**。具体的には:

- 正規の形式: TOML または JSON Schema
- 正規の場所: authority の公式リポジトリ内のバージョン管理されたパス
  （例: ato.run は `docs/rfcs/accepted/CAPSULE_HANDLE_SPEC.md` が policy v1）
- 配布: tooling は **bundled map** として `authority → schema` を同梱する。
  Runtime 起動時に fetch してはならない。
- 更新: tool の update で反映される。既発行の URL は tool update によって
  identity が変わってはならない（§3.1 invariant を保つ）。

Bundled map の canonical source（shared registry repo の所在、vendoring
方式、更新 cadence）は本 spec の範囲外であり、別文書
`DISCUSSION_AUTHORITY_SCHEMA_DISTRIBUTION.md` の結論を待つ。本 spec が
accepted に promote される時点で当該節は normative な参照に置き換わる。

**Rationale**: identity resolution が runtime-fetched state に依存すると、
同じ URL が時刻によって異なる capsule を指しうる。これは §3.1 の invariant
と衝突し、signing model を破壊する。

### 4.3 Versioning Independence

**Authority policy versioning is independent of URL spec versioning.**

- URL spec (本書) のメジャー改訂には authority 間の合意が必要
- Authority policy の改訂は当該 authority の裁量で可能
- 例: ato.run が将来 policy v2 で 3-segment path を採用しても、本 URL spec
  の改訂は不要

### 4.4 Localhost

`capsule://localhost:<port>/...`, `capsule://127.0.0.1:<port>/...`,
`capsule://[::1]:<port>/...` は **development authority として予約** する。
path semantics は local registry の実装が定める。production resolution の
対象としてはならない（default trust state は `Untrusted`、
`CAPSULE_HANDLE_SPEC.md §6` 参照）。

## 5. Resolution Model

### 5.1 Resolution Protocol

**Resolution** は capsule URL から capsule manifest を得る過程。resolution
は authority-specific resolver が担う。Generic な resolver は本 spec では
定めない。

- `ato.run` authority → `ato-store` HTTP API を使う ato.run resolver
- `github.com` authority → 将来的に GitHub API を使う github resolver。
  **Reference implementation (`ato-cli`) は v0.1 時点でこれらの URL を
  reject する。** 他の runtime が independent に github resolver を実装
  することは本 spec の範囲外であり、禁止しない（URL grammar は §2 に
  従って合法である）。
- `localhost:<port>` authority → local registry resolver

### 5.2 No Runtime Discovery

Runtime は identity 解釈のために HTTPS round-trip してはならない。
`.well-known/capsule-configuration` 等の discovery mechanism は **本 spec
v1 では定義しない**。将来 v0.2 で機能 endpoint（manifest URL template、
signing root 等）の discovery を **identity 解釈から分離した形で** 導入する
可能性を予約する。

## 6. Tooling Requirements

### 6.1 Authority-Agnostic Parsers

URL parser は authority policy を知らなくても以下を行えなければならない:

- §2 grammar の検証
- `authority` の抽出
- `path` segment 列の抽出（意味は解釈しない）
- `version-id` の抽出

§3.2 で禁止される mutable reference の syntactic 検出は、authority schema
の `version_id` pattern に依拠する（§6.2）。Authority schema を持たない
parser は grammar 違反のみ検出でき、semantic 違反（`@latest` 等）は
authority-aware tooling が担う。

### 6.2 Authority-Aware Tooling

Authority policy を使う tooling（CLI error message, IDE 補完, lockfile
diff, publisher badge 等）は、bundled authority schema map を参照する。
schema が bundled map に存在しない authority については authority-agnostic
な挙動に fall back する。

## 7. Deprecated Aliases

### 7.1 `capsule://store/`

`capsule://store/<path>` は `capsule://ato.run/<path>` の deprecated alias。

- parse 時に正規化される（§2.3）
- 既存 manifest / lockfile は次回更新時に `capsule://ato.run/` へ
  migration することを推奨する
- **v1.x 系列では受理を継続する。** 本 spec の次回メジャー改訂（v2）
  時点で削除する。

## 8. Migration Path from v0.1 Legacy Behavior

現行 `handle.rs` (2026-04-21 時点) は本 spec と以下の点で乖離する:

| 項目 | 現行 | 本 spec | Migration |
|------|------|---------|-----------|
| path segment 数 | 2 固定（hardcoded） | authority-defined | ato.run policy v1 で 2 固定を MUST と明記、grammar は緩める |
| `RESERVED_PUBLISHERS` | `handle.rs` 内 | authority policy | `apps/ato-store` の publisher registration validator に移管 |
| `@version` grammar | exact semver (spec) | point-in-time identity (spec) + authority syntax | CAPSULE_HANDLE_SPEC.md §4.1 を point-in-time に書き換え |
| `capsule://store/` | alias 受理 | deprecated alias（同じ） | 維持 |

Migration は段階的に行う:

1. 本 RFC を draft → accepted
2. `CAPSULE_HANDLE_SPEC.md` を "ato.run authority policy v1" にリネーム・
   再構成し、2-segment MUST を明示
3. `handle.rs::RESERVED_PUBLISHERS` を削除し、`apps/ato-store` の
   publisher registration 時に検証
4. `handle.rs::CanonicalHandle::RegistryCapsule` の `publisher/slug` 2-field
   shape は v1 policy の要求として維持（grammar 上は `Vec<String>` が
   正しいが、現実装との互換性のため保留）

## 9. Open Questions

本 draft で未決定の点:

- **Q1**: Authority schema の具体的な TOML/JSON Schema フォーマット
  （§4.1 の項目列挙のみ、構造は未定義）
- **Q2**: Bundled map の更新 cadence（tool release と同期か、別 channel か）
- **Q3**: `version-id` に含められる文字集合の厳密な定義（現在は "non-`/`,
  non-`@`, non-WSP, non-CTL" のみ）
- **Q4**: Future extension: `.well-known/capsule-configuration` で機能
  endpoint を配布する場合の具体的なスキーマ（v0.2 で議論）

これらは関連 Issue で追跡する。

## 10. References

- `docs/rfcs/accepted/CAPSULE_HANDLE_SPEC.md` — ato.run authority policy v1
- `docs/rfcs/accepted/SIGNATURE_SPEC.md` — signing model（point-in-time
  identity に依存）
- `docs/rfcs/accepted/IDENTITY_SPEC.md` — identity model
- RFC 3986 — URI Generic Syntax
