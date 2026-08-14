---
title: "ADR-002: Signature format (JCS) & scope"
status: accepted
date: 2026-01-29
author: "@egamikohsuke"
related: []
---

# ADR-002: Signature format (JCS) & scope

## Context
署名の正規化方式が Cap'n Proto と JCS で分岐しており、仕様と実装が不整合だった。
`.capsule` v2 では JCS が既に前提となっているため、UARC 署名の正規化方式を統一する必要がある。

## Decision
- 署名正規化は **JCS (RFC 8785)** を採用する。
- 署名対象は **manifest + sync payload/metadata** を含む。
  - 大きなバイナリは、署名ペイロード内にハッシュ（例: `manifest_hash`, `sync_payload_hash`, `sync_metadata_hash`）として含める。
- 署名検証失敗時は **拒否** する。
- 既存資産との互換は **必須ではない**（必要に応じて任意対応）。

## Consequences
- Cap'n Proto canonical bytes を署名入力として使う実装はレガシー扱いとなる。
- 仕様・検証フロー・ドキュメントを JCS に合わせて更新する。
