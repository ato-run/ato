---
title: "Capsule Sync Spec (capsule-sync)"
status: accepted
date: "2026-02-17"
author: "@egamikohsuke"
ssot:
  - "apps/capsule-sync/"
related: []
---

# Capsule Sync Spec (capsule-sync)

## 1. 概要
- `.sync` アーカイブのパーサ / VFS / Guest Protocol を提供するライブラリ。
- Guest Mode / `.sync` Runtime の基盤実装。

## 2. `.sync` アーカイブ構造
- ZIP 形式
- 必須エントリ:
  - `manifest.toml`
  - `payload`（Stored / no compression）
  - `sync.wasm`
- 任意エントリ:
  - `context.json`
  - `sync.proof`

## 3. Manifest
- `sync`: version/content_type/display_ext
- `meta`: created_by/created_at/hash_algo
- `policy`: ttl/timeout
- `permissions`: allow_hosts/allow_env
- `ownership`: owner_capsule/write_allowed
- `verification`: enabled/vm_type/proof_type
- `NetworkScope`: Local / Wan

## 4. VFS Mount
- `VfsMountConfig`: mount_path / expose_as_read_only / show_original_extension
- `VfsEntry`: payload の offset / size を持つ read-only 参照

## 5. Guest Protocol
- `GUEST_PROTOCOL_VERSION = guest.v1`
- `GuestContext`:
  - mode: Widget | Headless
  - role: Consumer | Owner
  - permissions: read/write/execute + allowlist
- `GuestAction`:
  - ReadPayload / ReadContext / WritePayload / WriteContext / ExecuteWasm / UpdatePayload
- `GuestResponse`:
  - ok / result / error

## 6. TTL
- `manifest.policy.ttl` を `created_at` から判定。
- `expires_in` / `is_expired` を提供。

## 7. Payload 更新
- `SyncArchive::update_payload()`
  - `payload` を置換し再構築。

## 8. Builder
- `SyncBuilder`:
  - manifest/payload/context/wasm/proof をまとめて `.sync` 生成。

## 9. SharePolicy
- `NetworkScope::Local` → LogicOnly
- `NetworkScope::Wan` → VerifiedSnapshot

## 10. 依存
- zip / blake3 / serde / toml / base64 / chrono
