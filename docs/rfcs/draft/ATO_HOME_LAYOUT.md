# Ato Home Layout

**Status:** Draft decision note  
**Date:** 2026-05-02  
**Scope:** `~/.ato` の local storage layout、lifetime 境界、desktop/system capsule の trust class、既存 layout からの移行方針  
**Related:** [CAPSULE_FOUNDATION_RESOURCE_STATE_MEMO.md](CAPSULE_FOUNDATION_RESOURCE_STATE_MEMO.md), [FOUNDATION_PROFILE_AND_PLACEMENT.md](../accepted/FOUNDATION_PROFILE_AND_PLACEMENT.md), [UNIFIED_EXECUTION_MODEL.md](UNIFIED_EXECUTION_MODEL.md)

## 1. 結論

`~/.ato` は、ユーザーから見える主要な状態を次の 4 つに整理する。

1. `store/`: 検証済み immutable object とその name index
2. `state/`: capsule lifetime の persistent mutable state
3. `runs/`: session lifetime の active/terminated execution state
4. root control surface: `config.toml`, `trust.toml`, `policy.toml`, `keys/`

現行実装にある `apps/ato-desktop/`, `run/`, `runs/`, `cache/run-host/`, `gh-install/`, `tmp/gh-run/`, `runtimes/`, `toolchains/`, `userland/` は、この 4 分類へ段階的に吸収する。

最終形:

```text
~/.ato/
├── config.toml
├── trust.toml
├── policy.toml
├── keys/
│
├── store/
│   ├── blobs/<hash>/
│   ├── capsules/<capsule-id>/<version>/
│   └── runtimes/<name>/<version>/<platform>/
│
├── state/
│   ├── _global/
│   │   └── userland/
│   └── <slug>-<short-hash>/
│       ├── identity.json
│       ├── bindings.json
│       ├── data/
│       └── userland/
│
└── runs/
    └── <kind>-<uuidv7>/
        ├── session.json
        ├── audit.json
        ├── ipc.sock
        ├── pid
        ├── deps/
        ├── tmp/
        ├── cache/
        └── log
```

`runs/<id>/session.json` の `state` field で `active` / `terminated` を区別する。現時点では `runs/active/` や `archives/` を top-level に追加しない。

## 2. 原則

### 2.1 Lifetime で分ける

| Root | Lifetime | Mutable | GC / cleanup |
| --- | --- | --- | --- |
| `store/` | object が参照される限り | No | refs / gcroots から到達不能なら GC 可 |
| `state/` | capsule ownership が続く限り | Yes | uninstall / user action / policy で整理 |
| `runs/` | session record が保持される限り | Yes | `session.state`, `terminated_at`, retention policy で整理 |
| root control surface | user profile が続く限り | Yes | user action のみ |

`cache` は top-level mental model にしない。短命 cache は `runs/<id>/cache/`、検証済み immutable artifact は `store/blobs/<hash>/` へ昇格する。

### 2.2 Everything is a capsule は privilege flattening ではない

`Everything is a capsule` は、同じ handle、model、lifecycle で扱うことを意味する。すべての capsule を同じ trust class に置くことは意味しない。

従って `ato-desktop` は capsule として扱うが、ordinary capsule ではない。`ato-desktop` は system capsule trust class に属し、host control plane capability は self-declaration ではなく署名済み grant によってのみ与えられる。

正しい分離:

| 観点 | ordinary capsule | system capsule (`ato-desktop` など) |
| --- | --- | --- |
| Handle | 同じ capsule handle model | 同じ capsule handle model |
| Store / state / runs | 同じ layout | 同じ layout |
| Trust class | `ordinary-capsule` | `system-capsule` |
| High authority grant | 原則不可 | issuer 署名が必要 |
| Policy enforcement | capability grant に従う | system grant と subject digest に従う |

## 3. Root ごとの責務

### 3.1 `store/`

`store/` は immutable class (`capsule`, `foundation`, `resource`) の storage root である。真の正本は `blobs/<hash>/` に置く。

```text
store/
├── blobs/<hash>/
├── capsules/<capsule-id>/<version>/
└── runtimes/<name>/<version>/<platform>/
```

`capsules/` と `runtimes/` は name-addressed index であり、content-addressed store 本体ではない。index entry は blob digest、signature/provenance、source handle、version、platform などを指す。

この分離により、次の混乱を避ける。

- `publisher/slug/version` を content-addressed と誤解しない
- capsule artifact と runtime/toolchain artifact を同じ immutable model で扱う
- install と execution materialization を分離する

### 3.2 `state/`

`state/` は mutable class の storage root である。CAS object にしてはならない。

```text
state/
├── _global/
│   └── userland/
└── <slug>-<short-hash>/
    ├── identity.json
    ├── bindings.json
    ├── data/
    └── userland/
```

`<slug>-<short-hash>` は readable slug と canonical identity hash の短縮形で作る。URL 正規化だけを directory name に使わない。理由は、長さ、case sensitivity、private URL leakage、rename 追従の問題を避けるためである。

`data/` が state の実体である。`volumes/` は実体ディレクトリとしては置かない。state binding の意味、durability、mount policy、owner capsule、cleanup policy は `bindings.json` に記録する。

例:

```json
{
  "bindings": [
    {
      "name": "postgres",
      "path": "data/postgres",
      "kind": "database",
      "durability": "persistent",
      "cleanup": "on-uninstall-confirm"
    }
  ]
}
```

### 3.3 `runs/`

`runs/` は execution session の storage root である。active session と terminated session は filesystem tree ではなく `session.json` の metadata で区別する。

```text
runs/
└── <kind>-<uuidv7>/
    ├── session.json
    ├── audit.json
    ├── ipc.sock
    ├── pid
    ├── deps/
    ├── tmp/
    ├── cache/
    └── log
```

`session.json` 例:

```json
{
  "schema_version": 1,
  "id": "desktop-01HXY...",
  "kind": "desktop",
  "state": "active",
  "capsule_id": "ato-desktop-4b2c9a10",
  "pid": 16184,
  "started_at": "2026-05-02T00:00:00Z",
  "terminated_at": null
}
```

Active-only files:

- `ipc.sock`
- `pid`

Terminated session でも保持してよい files:

- `session.json`
- `audit.json`
- `log`

`deps/` は provider-backed synthetic workspace や session-local dependency materialization の置き場である。`node_modules` や `.venv` を capsule source tree に直接生やさず、session の dependency root へ閉じ込める。

### 3.4 Root control surface

Root 直下には、ユーザー単位の control files と key fallback のみを置く。

```text
config.toml
trust.toml
policy.toml
keys/
```

責務:

| Path | Responsibility |
| --- | --- |
| `config.toml` | non-secret preferences のみ |
| `trust.toml` | TOFU, petnames, revocation, trusted issuers |
| `policy.toml` | isolation, network, capability override |
| `keys/` | OS Keychain が使えない環境の fallback |

Secrets を `config.toml` に混ぜない。Secrets は OS Keychain を優先し、file fallback が必要な場合も `keys/` または専用 encrypted store に分離する。

## 4. Trust class と grants

`state/<capsule-id>/identity.json` は canonical identity と trust class を保持する。

Ordinary capsule:

```json
{
  "canonical": "capsule://github.com/Koh0920/WasedaP2P@abc123",
  "publisher": "Koh0920",
  "slug": "wasedap2p",
  "trust_class": "ordinary-capsule",
  "trust_class_grants": []
}
```

System capsule:

```json
{
  "canonical": "capsule://ato.run/foundation/desktop@v0.5.0",
  "publisher": "ato.foundation",
  "slug": "ato-desktop",
  "trust_class": "system-capsule",
  "trust_class_grants": [
    {
      "name": "host_control_plane",
      "issuer": "ato.foundation",
      "subject_digest": "sha256:...",
      "scope": ["spawn", "ipc-broker", "display-routing"],
      "issued_at": "2026-05-02T00:00:00Z",
      "revocable": false
    }
  ]
}
```

Capability grant の不変条件:

1. Capsule の self-declaration は grant ではない。
2. High authority grant は issuer 署名と subject digest に紐づく。
3. `host_control_plane` は ordinary capsule に self-service で付与できない。
4. Filesystem layout は同じでも trust class は異なりうる。

## 5. 現行 layout からの対応表

| Current path | Target path | Notes |
| --- | --- | --- |
| `~/.ato/store/<publisher>/<slug>/<version>/` | `~/.ato/store/capsules/<capsule-id>/<version>/` + `store/blobs/<hash>/` | existing path は read fallback |
| `~/.ato/runtimes/` | `~/.ato/store/runtimes/` + `store/blobs/<hash>/` | runtime/toolchain は foundation artifact |
| `~/.ato/toolchains/` | `~/.ato/store/runtimes/` | tool/runtime distinction は metadata へ |
| `~/.ato/apps/ato-desktop/sessions/` | `~/.ato/runs/<desktop-uuid>/session.json` | desktop も session として扱う |
| `~/.ato/apps/ato-desktop/services/` | `~/.ato/state/<ato-desktop-id>/data/services/` | managed service state |
| `~/.ato/run/*.sock`, `*.pid` | `~/.ato/runs/<session-id>/ipc.sock`, `pid` | `run` と `runs` を統合 |
| `~/.ato/cache/run-host/` | `~/.ato/runs/<session-id>/cache/` | session-local cache |
| `~/.ato/gh-install/` | `store/blobs/<hash>/` or `runs/<id>/tmp/` | verified artifact は store、scratch は runs |
| `~/.ato/tmp/gh-run/` | `~/.ato/runs/<session-id>/deps/` | transient synthetic workspace |
| `~/.ato/userland/` | `~/.ato/state/_global/userland/` | global userland は explicit policy が必要 |
| `~/.ato/secrets.json` | OS Keychain or `keys/` fallback | plaintext root file は廃止 |
| `~/.ato/capsule-configs.json` | `config.toml` or `state/<capsule-id>/identity/config metadata` | non-secret only |
| `~/.ato/capsule-policy-overrides.json` | `policy.toml` | policy は config と分離 |

## 6. Migration policy

Migration は gradual strategy とする。

1. 新規 write は新 layout に行う。
2. Read は新 layout を優先し、旧 layout を fallback として読む。
3. `ato doctor` または `ato migrate home-layout` で旧 path の存在と移行状況を表示する。
4. 旧 path から新 path へ copy / index 作成する migration を用意する。
5. N release 後に旧 write path を削除する。
6. Destructive cleanup は user confirmation を必須にする。

Symlink redirect は標準戦略にしない。macOS 以外、権限、backup tooling、case sensitivity の差で layout invariant が崩れやすいため、互換は code fallback で扱う。

## 7. Open decisions

未確定事項:

1. `<short-hash>` の長さ: 8, 10, 12 hex のどれを標準にするか。
2. `store/blobs/<hash>/` の hash algorithm label: `sha256-...` か `blake3-...` か。
3. `trust_class_grants` を `identity.json` に埋めるか、`trust.toml` の ref として保持するか。
4. `runs/<id>/` の retention default: 7 日、30 日、manual のどれにするか。
5. `state/_global/userland/` を default 有効にするか、明示 opt-in にするか。

## 8. Non-goals

この document では次を決めない。

- Public Store API の schema
- Capsule manifest v0.3 の全 field
- OS Keychain backend の実装詳細
- Full CAS GC algorithm
- `ato-desktop` の UI/GPUI state model
- Existing user data の具体的 migration code

## 9. まとめ

今回の議論の結論は次の 3 点に要約される。

1. `~/.ato` は lifetime で分ける。Immutable は `store/`、persistent mutable は `state/`、session mutable は `runs/`、user-level control は root control surface に置く。
2. `Everything is a capsule` は privilege flattening ではない。`ato-desktop` も capsule として同じ layout に置くが、system capsule trust class と署名済み grant を持つ。
3. Active/terminated session と data binding の意味は filesystem hierarchy で増やしすぎず、metadata (`session.json`, `bindings.json`, `identity.json`) で表す。

この形により、現行の `run` vs `runs`、`apps/ato-desktop`、`cache/run-host`、`gh-install`、`runtimes/toolchains` の散らかりを、Ato の capsule/foundation/resource/state model に沿って段階的に畳み込める。