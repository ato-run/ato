---
title: "Session Surface Contract v1"
status: draft
date: "2026-07-14"
author: "@koh0920"
ssot:
  - "crates/protocol/src/session_surface.rs"
  - "crates/capsule/src/foundation/types/manifest.rs"
related:
  - "docs/rfcs/draft/PIXEL_STREAM_PROFILE_V1.md"
  - "docs/rfcs/accepted/CAPSULE_HANDLE_SPEC.md"
  - "docs/rfcs/draft/SURFACE_MATERIALIZATION.md"
---

# Session Surface Contract v1

## 1. 決定

session の表示方法を Snapshot 固有機能や Desktop 固有の
`display_strategy` ではなく、versioned `SessionSurface` contract として扱う。
選択は runner lease 発行前に行い、runner は materialize 直前に同じ交差を再検証する。

```text
capsule requirement × launch client acceptance × runner support
                              ↓
                    selected descriptor
                              +
                     rotatable access
```

責務は次のとおり分離する。

- Run UX ADR: `LaunchCardState`、待機、失敗、再接続を含む launch UX の SSOT
- 本 RFC: surface の requirement、capability negotiation、wire union、migration
- `PIXEL_STREAM_PROFILE_V1.md`: Linux pixel stream の具体的な transport と readiness
- `CAPSULE_HANDLE_SPEC.md`: resource identity と authority policy のみ
- `SURFACE_MATERIALIZATION.md`: Desktop shell 内の既存 WebView retention。Pixel transport の SSOT ではない

## 2. 宣言と解決

`capsule.toml` schema 0.3 の target が presentation requirement を宣言する。

```toml
[targets.desktop.surface]
kind = "pixel_stream"
profiles = ["ato.pixel-stream.v1"]
```

authoring field は `surface`、normalized manifest / lock / sealed artifact で扱う意味は
`surface_requirement` である。これは concrete URL や credential を含まない。

```json
{
  "kind": "pixel_stream",
  "profiles": ["ato.pixel-stream.v1"]
}
```

`kind` は v1 で `web | pixel_stream | terminal` の tagged union とする。
未知 kind は typed unsupported として保持できるが、選択・実行してはならない。
未知 kind を Web と解釈してはならない。

## 3. Capability advertisement

launch client は request に `accepted_session_surfaces` を含める。

```json
{
  "accepted_session_surfaces": [
    { "kind": "web", "profiles": ["ato.web-surface.v1"] },
    { "kind": "pixel_stream", "profiles": ["ato.pixel-stream.v1"] }
  ]
}
```

runner heartbeat は `supported_session_surfaces` を含める。

```json
{
  "supported_session_surfaces": [
    {
      "kind": "pixel_stream",
      "profiles": ["ato.pixel-stream.v1"],
      "transports": ["rfb_websocket"]
    }
  ]
}
```

省略、空配列、未知値、malformed は別の状態であり、暗黙の同値化をしない。
explicit surface の場合はいずれも fail-closed とする。API は capsule × client × runner
の profile 交差が空でない runner にだけ lease を発行する。runner は sealed artifact の
`surface_requirement`、lease の `accepted_session_surfaces`、実機で有効な local support を
再交差し、lease descriptor と完全一致することを確認する。

Pixel support は feature 名だけで広告してはならない。gateway が設定済みで、対象 platform
と Ready-State backend が profile を実行できる場合にだけ広告する。

## 4. Descriptor と Access

descriptor は session lifetime 中 immutable、access は更新可能である。

```json
{
  "surface_contract_version": "1",
  "surface": {
    "descriptor": {
      "kind": "pixel_stream",
      "profile": "ato.pixel-stream.v1",
      "surface_id": "surface_01",
      "transport": "rfb_websocket",
      "viewport": { "width": 1280, "height": 720 },
      "capabilities": {
        "keyboard": "us",
        "pointer": true,
        "clipboard": false,
        "file_transfer": false,
        "audio": false
      }
    },
    "access": {
      "connect_url": "wss://session.example/surfaces/surface_01",
      "auth_exchange_url": "https://session.example/surfaces/surface_01/auth",
      "expires_at": "2026-07-14T12:00:00Z",
      "generation": 1
    }
  }
}
```

- `surface_contract_version` は envelope の解釈 version
- `surface_id` は session 内で immutable
- `connect_url` は absolute、token-free。Pixel v1 は `wss://`
- authenticated Pixel v1 は same-origin の absolute `auth_exchange_url` を必須とする
- `expires_at` と `generation` は access rotation を表す
- access、cookie、assertion、one-time grant を lock、snapshot、session record の descriptor 欄へ永続化しない

Web descriptor は `profile=ato.web-surface.v1` と `embed_policy`、Terminal descriptor は
`profile=ato.terminal-surface.v1` と `terminal_websocket` transport を持つ。

## 5. Lease contract

explicit surface の restore lease は次を必須とする。

```json
{
  "surface_contract_version": "1",
  "session_id": "session_01",
  "accepted_session_surfaces": [
    { "kind": "pixel_stream", "profiles": ["ato.pixel-stream.v1"] }
  ],
  "session_surface": {
    "kind": "pixel_stream",
    "profile": "ato.pixel-stream.v1",
    "surface_id": "surface_01",
    "transport": "rfb_websocket",
    "viewport": { "width": 1280, "height": 720 },
    "capabilities": {}
  }
}
```

lease の `session_surface` は descriptor のみを運び、access や credential を運ばない。
`session_id` は gateway scope と assertion claim を一致させる binding である。

## 6. Ready response の dual-read / dual-write

移行中の reader は次の順序を厳守する。

1. `surface` field が存在する場合、それを authoritative とする。
2. `surface` が null、malformed、unsupported の場合は失敗する。`app_url` へ fallback しない。
3. `surface` field 自体が存在しない場合だけ、legacy `app_url` を Web surface として合成できる。
4. legacy 合成には `app_expires_at` が必要で、既定 `embed_policy` は `sandboxed`。

writer は常に canonical `surface` を書く。移行期間中、Web に限り
`app_url` / `embed_policy` / `app_expires_at` を併記できる。Pixel/Terminal から
偽の `app_url` を生成してはならない。

## 7. Gateway authentication boundary

authenticated MVP（5A）は SPEC-G を待たずに実装する。guest 発行（5G）は別 gate であり、
`principal.kind=guest` は wire reservation のみとする。

- gateway assertion header: `X-Ato-Surface-Assertion`
- audience: `ato.runner.surface-gateway`
- claims: `{ aud, session_id, surface_id, principal, exp, jti, kid }`
- principal: `{ kind: "user" | "guest", id: string }`
- signature、`exp`、`kid`、`jti` replay、session/surface/principal binding を gateway で検証する
- assertion を query string、WebSocket URL、log に置かない

Browser credential boundary:

- exchange endpoint と WebSocket は同一 origin
- cookie は host-only、`HttpOnly`、`Secure`、`SameSite=Lax`
- wildcard domain cookie は禁止
- WebSocket handshake の `Origin` を明示 allowlist で検証
- browser cookie / assertion を guest VM や RFB server へ転送しない
- auth exchange response は `Cache-Control: no-store`
- URL は credential-free とし、access expiry 後は再 exchange する

## 8. EndpointContract

Ready-State artifact は legacy `ports` に加え、typed endpoint を持てる。

```json
{
  "role": "pixel_rfb",
  "protocol": "tcp",
  "exposure": "guest_private",
  "port": 5900,
  "readiness": { "kind": "first_frame" }
}
```

field は `role / protocol / exposure / port / readiness`。`exposure` に serde default を
設けず、省略・未知値を deserialize 時点で拒否する。`pixel_rfb` と `guest_control` は
`public_proxy` にできない。Pixel は TCP accept だけでは ready ではなく、gateway が最初の
framebuffer update を観測した `first_frame` を readiness とする。

## 9. State ownership

- declaration: `capsule.toml`
- resolution: `ato.lock.json` の target `surface_requirement`
- artifact identity: sealed `ReadyStateManifest.surface_requirement`
- session state: immutable descriptor
- ephemeral gateway state: access、cookie、assertion、replay cache

これらを混在させない。surface requirement を変更すると artifact manifest identity も変わる。

## 10. Rollout と受入条件

1. protocol DTO と parser を additive に導入
2. capsule target→lock→sealed artifact→builder ack を伝播
3. API が client/runner capability を含めて lease 前 negotiation
4. runner が実機 capability で再 negotiation
5. authenticated gateway と PWA viewer を有効化
6. Web dual-write を維持したまま telemetry で legacy reader を確認
7. SPEC-G 承認後にのみ guest issuance を追加

最低受入条件:

- Web legacy response は引き続き読める
- canonical field が malformed のとき legacy fallback しない
- Pixel lease は version、session id、client capabilities、descriptor の欠落を拒否
- runner capability 不一致を restore 前に拒否
- unknown kind/profile/transport/exposure を typed error で拒否
- Pixel endpoint は private/internal + `first_frame`
- snapshot、lock、URL、log に credential が残らない
