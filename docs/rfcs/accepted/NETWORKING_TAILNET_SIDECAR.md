---
title: "Tailnet / Sidecar Networking（最新）"
status: accepted
date: "2026-02-17"
author: "@egamikohsuke"
ssot:
  - "apps/ato-tsnetd/"
  - "apps/ato-cli/core/src/tsnet/"
related: []
---

# Tailnet / Sidecar Networking（最新）

NAT越え通信とプロキシ強制のアーキテクチャをまとめるドキュメント。
> 旧 ADR `docs/adr/2026-01-17_000000_embedded-tailnet-sidecar.md` は廃止済み。ソースコードの実装 (`apps/ato-tsnetd/`, `apps/ato-cli/core/src/tsnet/`) を正本とする。

## 1. 目的

- クライアント（`ato-desktop` / `ato-cli`）とエージェント（`nacelle`）の間で、**NAT越えのセキュアなP2P接続**を確立する。
- Root/Sudo不要（userspace networking）。
- ユーザーに追加VPNインストールを要求しない。

## 2. 採用: tsnet sidecar（A案）

- Go `tsnet` を用いた `ato-tsnetd` を sidecar として同梱。
- Rust側はプロセス管理 + IPC（UDS/Named Pipe + gRPC）で制御。
- Headscale は独立サービスとして運用し、Control Plane が PreAuthKey を発行する。

## 3. プロキシ（SOCKS5）と強制

- `ato-tsnetd` は SOCKS5 を提供し、Capsuleの外向き通信はこの経路を使用する。
- `nacelle` はOS隔離で「sidecar以外の外向き経路」を遮断し、強制する。

参照（強制モデル）: `SECURITY_AND_ISOLATION_MODEL.md` Section 4

## 4. 典型フロー（ペアリング）

1. Desktop/CLI が Control Plane に pairing を要求
2. Control Plane が Headscale に PreAuthKey を作成
3. Desktop/CLI が `ato-tsnetd` を起動し AuthKey で登録
4. tailnet 上で Agent へダイヤルし、P2P/DERPで接続

## 5. 実装上の注意

- sidecar は追加バイナリになる（サイズ増）。
- IPCとプロセス監視（再起動・終了伝播）の設計が重要。
- ログは親プロセス側に集約し、UIで観測できるようにする。
