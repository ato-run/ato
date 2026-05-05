---
title: "セキュリティ & 隔離モデル（最新）"
status: accepted
date: "2026-02-17"
author: "@egamikohsuke"
ssot:
  - "apps/nacelle/src/system/"
related:
  - "CAPSULE_CORE.md"
  - "ARCHITECTURE_OVERVIEW.md"
  - "CAPSULE_FORMAT_V2.md"
  - "SIGNATURE_SPEC.md"
---

# セキュリティ & 隔離モデル（最新）

**Capsule実行時のセキュリティ境界**を定義するドキュメント。
> 旧 `docs/adr/` は廃止済み。ソースコードの実装を正本とする。

## 1. 責務分離（最重要）

- `ato-cli` / `ato-desktop`（ホスト）
  - 検証・署名・権限チェック・UX
  - ポリシー決定（何を許可するか）

- `nacelle`（実行エンジン）
  - サンドボックス適用（どう隔離するか）
  - Supervisor（プロセス起動・監視・終了）
  - socket activation（FD継承）

参照: `CAPSULE_CORE.md` Section 9 (routing), `ARCHITECTURE_OVERVIEW.md` Section 1

## 2. 署名と検証

- 配布物（`.capsule`）は `signature.json` を用いて Ed25519 署名し、JCS（RFC 8785）で正規化して検証する。
- `manifest_hash` と `payload_hash` の二重ハッシュで改ざん耐性を持つ。

参照: `CAPSULE_FORMAT_V2.md`, `SIGNATURE_SPEC.md`

## 3. 環境変数の扱い（reconstructed baseline）

ランタイムはホスト環境変数を **暗黙に継承しない**。

- まず環境をクリア
- 最小ベースライン（`PATH`, locale vars, reconstructed `HOME` / `TMPDIR`, proxy / CA vars, `CAPSULE_*` など）を再構成
- `execution.env` を最後に適用（最優先）

このモデルは「完全な空 env」ではなく、「再構成された isolation baseline」を正本とする。

### 3.1 Secret 分類と注入経路

- `user_secret`（ユーザー提供秘密: API key など）は env 直接注入を禁止し、FD（`pipe(2)` / `memfd_create(2)`）で受け渡す。
- `session_token`（短命セッショントークン: 例 `ATO_BRIDGE_TOKEN`）は allowlist 管理下で env 注入を許可する。
- 詳細契約は `SECRET_CLASSIFICATION_SPEC.md` を正本とする。

参照: `CAPSULE_CORE.md` Section 7.2 (IsolationConfig)

## 4. ネットワーク（deny-by-default + 強制経路）

### 4.1 許可される経路（OS API Host Bridge ADR）

実行時に許可されるネットワーク/IPC経路は原則2つだけ:

1. **Host Bridge IPC**（stdio または UDS）
2. **Sidecar Proxy**（localhost TCP, SOCKS5）

それ以外の egress は `nacelle` が遮断する。

参照: `NETWORKING_TAILNET_SIDECAR.md`

### 4.2 ドメイン allowlist と解決の責務

- allowlist の解決や検証は「ビルド時に寄せる」（Smart Build）
- 実行時のDNS追従は限定的（起動時解決など）

参照: `ARCHITECTURE_OVERVIEW.md` Section 1 (Smart Build, Dumb Runtime)

## 4.3 Filesystem grants

- ホスト filesystem 追加アクセスは `--read`, `--write`, `--read-write` で明示付与する
- grant 解決は呼び出し側 cwd 基準で正規化する
- symlink traversal を含む grant は拒否する

## 5. OS別実装（system abstraction）

OSネイティブの隔離（eBPF/WFP/PF 等）は `system` モジュールの trait 経由で提供し、コア実行コードからOS分岐を隔離する。

参照: `apps/nacelle/src/system/` モジュール

## 5.1 Engine discovery

- nacelle の探索順は `--nacelle` → `NACELLE_PATH` → manifest / compat engine setting → user config default → portable mode
- PATH search はセキュリティ上の理由で無効

## 6. OS APIアクセス（Host Bridge Pattern）

- Capsule がOS APIを直接叩くことは許可しない。
- OS機能はホスト（CLI/Desktop）が提供し、RPCで呼び出す。
- 認証は短命トークン（例: `ATO_BRIDGE_TOKEN`）を環境変数で注入し、Host側で検証する。

参照: `ARCHITECTURE_OVERVIEW.md` Section 1 (Host Bridge Pattern)

## 7. 付記: 薄いCapsule / 太いコンテナ

- `build.exclude_libs` により、ホスト/コンテナ側にある巨大依存（GPU libs, ML frameworks等）をCapsuleから除外できる。
- その場合でも env の透過は allowlist で明示し、`LD_LIBRARY_PATH` 等の取り扱いは慎重にする。

参照: `CAPSULE_CORE.md` Section 7.2 (IsolationConfig)
