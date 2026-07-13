---
title: "Pixel Stream Profile v1"
status: draft
date: "2026-07-14"
author: "@koh0920"
ssot:
  - "crates/protocol/src/session_surface.rs"
related:
  - "docs/rfcs/draft/SESSION_SURFACE_CONTRACT.md"
  - "docs/snapshot.md"
---

# Pixel Stream Profile v1

## 1. Scope

`ato.pixel-stream.v1` は、HTTP UI を持たない Linux 2D GUI capsule を Ready-State
snapshot から復元し、browser 内で操作可能な pixel stream として提示する profile である。
Snapshot の機能分類ではなく `SessionSurface(kind=pixel_stream)` の一 profile とする。

v1 の support matrix:

- guest: Linux x86_64
- display: Xvfb + lightweight window manager（Openbox）
- rendering: software rendering
- viewport: fixed 1280×720
- frame rate: maximum 30 fps
- input: pointer + US keyboard
- transport: guest-private RFB、host-side RFB-over-WebSocket

GPU、Wayland、macOS/Windows guest、動的 resize は v1 対象外。

## 2. Descriptor

```json
{
  "kind": "pixel_stream",
  "profile": "ato.pixel-stream.v1",
  "surface_id": "surface_01",
  "transport": "rfb_websocket",
  "viewport": { "width": 1280, "height": 720 },
  "capabilities": {
    "pointer": true,
    "keyboard": "us",
    "clipboard": false,
    "file_transfer": false,
    "audio": false,
    "dynamic_resize": false
  }
}
```

`transport` は `rfb_websocket` のみ。viewport は positive かつ v1 fixture では
1280×720。capability value は Boolean / integer / string の scalar に限定し、
任意 nested JSON を第二の protocol にしない。

## 3. Guest image contract

build image は少なくとも次を固定する。

- Xvfb と Openbox の version / package source
- RFB server とその起動 argv
- software rendering 用 package / env
- locale、timezone、font set、US keyboard mapping
- target app の command と working directory
- display number、RFB port、viewport

RFB listener は guest-private endpoint とし、外部 interface や public ingress へ直接公開しない。
VNC password は end-user security boundary として扱わない。browser の認証・認可は host gateway
で完結し、guest へ cookie、assertion、API credential を渡さない。

## 4. Snapshot phases

Build readiness:

1. Xvfb 起動
2. Openbox 起動
3. RFB server 起動
4. app process 起動
5. target window の WM_CLASS / title / mapped state を確認
6. framebuffer update を確認
7. secret-free gate 後に snapshot seal

Session readiness:

1. snapshot restore
2. authenticated WebSocket gateway を public session URL に起動
3. token-free URL、exact allowed `Origin`、one-time assertion header で public gateway へ接続
4. gateway 越しに RFB handshake、button なし pointer event、framebuffer request を順に送る
5. 同じ ordered stream で最初の完全な framebuffer update を受信
6. probe assertion を破棄し ready response を返す

private TCP connect や gateway listener の bind 成功だけを ready としてはならない。public ingress、
Origin/assertion authorization、browser→RFB input、RFB→browser first frame の全経路が成功しなければ
gateway を停止して session を失敗させる。endpoint contract は次を必須とする。

```json
{
  "role": "pixel_rfb",
  "protocol": "tcp",
  "exposure": "guest_private",
  "port": 5900,
  "readiness": { "kind": "first_frame" }
}
```

`host_internal` は gateway 内部 relay に使用できるが `public_proxy` は禁止。

## 5. Input and browser transport

RFB bytes は host gateway が WebSocket へ bridge する。PWA は `wss://` の token-free URL と
same-origin auth exchange を使用する。v1 input は pointer と US keyboard のみ。

以下は明示的に無効:

- clipboard sync
- file transfer
- audio
- drag-and-drop upload
- RFB credential UI
- IME / composition fidelity guarantee
- dynamic resize
- multi-monitor
- accessibility tree forwarding

browser accessibility は pixel canvas のため限定的であり、同等の Web/semantic surface がある場合は
そちらを優先できるよう別 profile として宣言する。

## 6. Security invariants

- guest RFB port は internet から到達不能
- gateway assertion の audience/session/surface/principal を検証
- one-time exchange と WebSocket は同一 origin
- host-only secure cookie と Origin allowlist
- assertion、cookie、access grant を snapshot / CAS / lock / URL / log に保存しない
- build 時に placeholder credential を焼き込まない
- RFB password を authorization の根拠にしない
- guest process は host-side principal を知らない

## 7. Lifecycle

- WebSocket disconnect は即 VM teardown を意味しない。短い reconnect grace を持てる
- access generation 更新時、descriptor / surface_id は変えない
- preview idle timeout は parsed RFB keyboard / pointer input のみで更新する。framebuffer
  request、keepalive、outbound frame は activity に数えない
- 動画の受動視聴などが timeout する挙動は MVP の既知制約とし、将来の activity policy と分離する
- hard TTL、idle policy、manual stop のいずれでも gateway listener、relay、VM、overlay を破棄
- partial failure 時も public listener を先に閉じる
- stale generation の再接続を拒否

## 8. Observability

credential を含めず次を計測する。

- restore start→guest RFB connect
- restore start→first frame
- first frame→WebSocket publish
- frame rate / encoded bytes / dropped frames
- input event count（内容は記録しない）
- disconnect / reconnect / teardown reason
- profile、transport、viewport、runner class

## 9. Acceptance

- Linux x86_64 fixture が snapshot restore 後に 1280×720 の非空 first frame を返す
- pointer click と US key input が guest app に届く
- TCP listener のみでは readiness が成立しない
- guest RFB へ public route が生成されない
- authenticated user 以外の WebSocket handshake を拒否
- Origin mismatch、expired assertion、replayed jti、session/surface mismatch を拒否
- clipboard/file/audio/RFB credential が UI と wire capability の双方で無効
- teardown 後に listener、VM、overlay、temporary access state が残らない
