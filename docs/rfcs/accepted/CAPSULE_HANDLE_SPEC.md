---
title: "ato.run Authority Policy v1"
status: accepted
date: "2026-04-21"
author: "@koh0920"
ssot:
  - "apps/ato-cli/core/src/handle.rs"
  - "apps/ato-store/src/routes/publishers.ts"
related:
  - "docs/rfcs/draft/CAPSULE_URL_SPEC.md"
---

# ato.run Authority Policy v1

> 本書は `capsule://` URL scheme における **ato.run authority に固有の
> Layer 2 policy** を定義する。URL grammar（Layer 1）は
> `CAPSULE_URL_SPEC.md` が定める。

## 1. Scope

本書は以下を定義する:

- ato.run authority 向けの canonicalization ルール
- surface mapping（CLI、Desktop omnibar）
- `@version-id` の point-in-time identity 要件（authority 横断の原則を
  ato.run の具体に落としたもの）
- ato.run 固有の予約 publisher 名
- snapshot semantics、registry identity、metadata/trust store、handle
  vs session display の関係

URL の grammar（scheme/authority/path/version-id の syntax）、mutable
reference 禁止の一般原則、authority policy の配布方式などは
`CAPSULE_URL_SPEC.md` の責務。本書はそれを前提に ato.run 固有の解釈を
与える。

## 2. Canonical Handle

canonical handle は `capsule://...` のみとする。ato.run 認可 capsule は
**2-segment path (`<publisher>/<slug>`) を MUST** とする。

- `capsule://github.com/<owner>/<repo>[@<commit-sha>]`
- `capsule://ato.run/<publisher>/<slug>[@<version>]`
- `capsule://localhost:<port>/<publisher>/<slug>[@<version>]`
- `capsule://127.0.0.1:<port>/<publisher>/<slug>[@<version>]`
- `capsule://[::1]:<port>/<publisher>/<slug>[@<version>]`

URL grammar の規定は `CAPSULE_URL_SPEC.md` (Layer 1) が担う。本書は ato.run
authority に関する Layer 2 policy を中心に扱い、github.com / localhost
については同スキームにおける参考表記として記載する。

次は invalid:

- `capsule://<publisher>/<slug>`
- `capsule://local/...`
- `ato://<publisher>/<slug>`

## 3. Surface Mapping

- CLI run surface:
  - `github.com/<owner>/<repo>`
  - `<publisher>/<slug>`
  - `<local-path>`
- CLI resolve surface:
  - terse ref
  - canonical `capsule://...`
  - local path
- Desktop omnibar:
  - canonical `capsule://...`
  - `<publisher>/<slug>` sugar
  - URL / search
- `ato://...`:
  - host route only
  - 例: `ato://auth/callback`, `ato://settings/...`

## 4. Canonicalization Rules

- `github.com/owner/repo` -> `capsule://github.com/owner/repo`
- `publisher/slug` -> `capsule://ato.run/publisher/slug`
- `capsule://ato.run/publisher/slug` -> canonical そのまま
- `capsule://github.com/owner/repo` -> canonical そのまま
- `capsule://localhost:8787/publisher/slug` -> canonical そのまま
- `capsule://store/publisher/slug` -> **deprecated alias** → `capsule://ato.run/publisher/slug` (see §10)
- `ato://...` は capsule handle としては解決しない
- `capsule://local/...` は採用しない

### 4.1 `@version-id` — Point-in-Time Identity

`@<version-id>` suffix は **point-in-time identity** を表す。すなわち
「任意の時点で、`<version-id>` を解決した結果は同じ単一の capsule
artifact を指す」という invariant を満たさねばならない（`CAPSULE_URL_SPEC.md`
§3.1, §3.2 参照）。

Mutable reference は禁止される。以下は authority を問わず **無効**:

- Range operator (`^1.2`, `~1.2`, `>=1.2`)
- Wildcards (`*`, `1.2.*`)
- Floating alias (`latest`, `stable`, `nightly`)
- Mutable git refs (branch 名, moving tag)

`<version-id>` の具体的な文法は authority が定める (Layer 2 policy):

**ato.run authority** — exact semver のみ有効:

```
version-id = MAJOR "." MINOR "." PATCH [ "-" prerelease ] [ "+" build ]
```

- ✅ `capsule://ato.run/acme/app@1.2.3` — exact release
- ✅ `capsule://ato.run/acme/app@1.2.3-beta.1` — pre-release
- ❌ `capsule://ato.run/acme/app@^1.2` — range operator
- ❌ `capsule://ato.run/acme/app@latest` — floating alias
- ❌ `capsule://ato.run/acme/app@~1.2` — tilde range

**github.com authority** — git commit SHA (40-hex) のみ immutable として有効:

- ✅ `capsule://github.com/acme/app@a1b2c3d4e5f6789012345678901234567890abcd`
- ❌ `capsule://github.com/acme/app@main` — mutable branch
- ❌ `capsule://github.com/acme/app@v1.2.3` — git tag は移動しうるため
  point-in-time 違反となる可能性（必要なら対応する commit SHA に resolve
  した形で lockfile に保存する）

`@<version-id>` を省略した場合は「最新の安定リリース」として resolve
する。これは resolution 時の解釈であり URL identity には含まれない。
バージョン固定が必要な場合は lockfile の `resolved_version` フィールドを
使う。

### 4.2 Reserved Publisher Names (ato.run policy)

以下の publisher 名は ato.run の first-party 用途または URI ルーティング
の曖昧性排除のため予約されており、ユーザー登録不可とする。これは
**ato.run authority 固有の policy** であり、他 authority には適用されない。

```
search  topic  user  store  api  registry  help  docs  status
```

検証箇所: `apps/ato-store` の publisher registration validator
(`src/routes/publishers.ts`)。ato-cli 側の URL parser は authority-agnostic
であり、この予約リストを持たない。

## 5. Handle Identity and Snapshot Identity

- handle は resource identity。
- snapshot は execution-time concrete identity。

GitHub:

- handle: `capsule://github.com/owner/repo`
- resolved snapshot: `{ commit_sha, default_branch, fetched_at }`

Registry:

- handle: `capsule://ato.run/publisher/slug[@version]`
- resolved snapshot: `{ version, release_id|content_hash, fetched_at }`

Loopback registry:

- handle: `capsule://localhost:8787/publisher/slug[@version]`
- resolved snapshot: `{ version, release_id|content_hash, fetched_at }`
- host-side resolution fetch で metadata を取得する
- guest runtime permission とは分離して扱う

LaunchPlan, preview, promotion, materialization, trust evaluation は snapshot 単位で扱う。

## 6. Registry Identity Model

external canonical form は当面 `ato.run` を表示 authority とする。  
ただし internal model では次を分離する。

- `display_authority`
- `registry_identity`
- `registry_endpoint`

これにより mirror / private registry / host rename に対応する。

loopback registry authority は developer-mode authority として扱い、trust state の既定値は `Untrusted`、initial isolation は fail-closed とする。

## 7. Metadata Cache and Trust Store

### 7.1 Resolved Metadata Cache

- canonical handle
- normalized input
- manifest summary
- resolved snapshot
- fetched_at
- TTL metadata
- registry/source adapter identity

### 7.2 Local Trust State Store

- trusted / untrusted / promoted / local
- session-scoped grant
- persistent allow/deny
- decision provenance
- local timestamps

metadata cache と local trust decision は同じ責務にしない。

## 8. CLI Compatibility

- `ato run capsule://...` は Phase 1 では reject。
- `ato resolve capsule://...` は accept。
- `ato app resolve` / `ato app session start` は internal control-plane surface として canonical handle を accept してよい。

## 9. Handle Identity vs Session Display

- handle は resource identity、snapshot は execution-time concrete identity とする。
- Desktop は handle から直接 runtime を決め打ちしない。`ato app session start` が返す session envelope の `display_strategy` を最終採用する。
- `display_strategy` は handle spec に従属する表示ヒントであり、少なくとも `guest_webview`, `web_url`, `terminal_stream`, `service_background`, `unsupported` を持つ。
- `runtime=web` capsule は `web_url` strategy を返してよく、`metadata.desky_guest` を必須にしない。

## 10. Deprecated Aliases

### `capsule://store/`

`capsule://store/<publisher>/<slug>` は `capsule://ato.run/<publisher>/<slug>` の非推奨エイリアス。

- 入力として受け付けるが、内部表現は常に `capsule://ato.run/` に正規化する。
- `capsule.toml` や lockfile で `capsule://store/` を使用しているものは次の更新タイミングで `capsule://ato.run/` に書き換えることを推奨する。
- 将来のメジャーバージョンで削除予定。

