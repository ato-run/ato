---
title: "Capsule Artifact Format v2（2026-01-18）"
status: accepted
date: "2026-01-18"
author: "@egamikohsuke"
ssot:
  - "apps/ato-cli/core/src/packers/capsule.rs"
related:
  - "SIGNATURE_SPEC.md"
---

# Capsule Artifact Format v2（2026-01-18）

**配布物（.capsule）の標準フォーマット**を実装・運用向けに要約したもの。
> 旧 ADR `docs/adr/2026-01-18_000001_capsule-format-v2.md` は廃止済み。ソースコードの実装 (`apps/ato-cli/core/src/packers/capsule.rs`) を正本とする。

## 1. 目的

- 署名と検証を **ストリーミングで可能**にする（先頭で manifest + signature を読める）。
- シングルファイル配布を維持しつつ、フォーマット破壊を避ける。
- 署名対象の曖昧さ（JSON順序・空白）を排除する。

## 2. コンテナ形式（外側）

- `.capsule` は **PAX TAR（POSIX.1-2001）**。
- 外側TARは **厳密なエントリ順**を持ち、以下を **この順で**含む:

1. `capsule.toml`（UTF-8 manifest、distribution metadata 付き）
2. `capsule.lock.json`（ロックファイル）
3. `sbom.spdx.json`（SPDX-2.3 形式の SBOM）
4. `signature.json`（RFC 8785 JCS で正規化する JSON 署名メタ、SBOM ハッシュ参照を含む）
5. `payload.tar.zst`（Zstd 圧縮された内側 TAR）
6. `payload.v3.manifest.json`（オプション、CAS チャンク manifest、`ATO_EXPERIMENTAL_V3_PACK=1` 時のみ）
7. `README.md`（オプション、nearest ancestor から自動取得）

> `packers/capsule.rs:30-39` — アーカイブ構造コメントと実装

外側TARは **非圧縮（または低圧縮）**を前提にし、二重圧縮とストリーミング性の劣化を避ける。

## 3. 署名（signature.json）

- `signature.json` は **RFC 8785 JCS** で canonicalize して署名/検証する。
- canonicalize対象から `signature` フィールドは除外する。

スキーマ（要点）:

```json
{
  "version": 1,
  "alg": "ed25519",
  "key_id": "did:key:<multicodec-ed25519-pub>",
  "signed_at": "<RFC3339>",
  "manifest_hash": "sha256:<HEX>",
  "payload_hash": "sha256:<HEX>",
  "signature": "<BASE64>"
}
```

- `manifest_hash`: `capsule.toml` のバイト列SHA-256
- `payload_hash`: **圧縮済み** `payload.tar.zst` のバイト列SHA-256

## 4. ペイロード（内側）

- `payload.tar.zst` は「内側TARをZstd圧縮」したもの。
- 再現性のため、内側TARエントリの `mtime` は固定（epoch 0 など）を推奨。

## 5. 展開時セキュリティ

`open`/`unpack` 実装は少なくとも以下を拒否する:

- 絶対パス
- `../` 等のディレクトリトラバーサル
- シンボリックリンク/ハードリンク（許可するなら抽出root内への拘束と検証が必須）
- デバイスファイル（FIFO/キャラクタ/ブロック）

## 6. 検証フロー（MUST）

1. `capsule.toml` と `signature.json` を先に読む
2. `manifest_hash` が一致することを検証
3. `signature.json` を JCS canonicalize（signature除外）して署名検証
4. `payload.tar.zst` をストリーミングしつつ hash 計算・抽出し、`payload_hash` を検証
5. 不一致なら中断し、抽出結果をロールバックする

## 7. 他仕様との関係（注意）

- 現行仕様は **`.capsule` 内の `signature.json`** を正とする。
  - 旧 ADR に記載されていた「sidecar JSON signature only」方式は廃止。
  - ただし、将来/別アーティファクト（例: 旧bundle、メタデータ）で「sidecar署名」を併用する余地はある。
- 署名仕様の詳細は `SIGNATURE_SPEC.md` を参照。
- JCS 正規化の決定は `2026-01-29_000002_signature-format-jcs.md` (ADR) を参照。
