---
title: "Runtime & Build Model（最新）"
status: accepted
date: "2026-02-17"
author: "@egamikohsuke"
ssot:
  - "apps/ato-cli/core/src/router.rs"
  - "apps/nacelle/src/launcher/"
related: []
---

# Runtime & Build Model（最新）

`targets.source` を中心とした **ビルド時/実行時の責務境界**を要約するドキュメント。
> 旧 `docs/adr/` は廃止済み。関連する設計の正本:
> - Smart Build, Dumb Runtime: `ARCHITECTURE_OVERVIEW.md` Section 1, `apps/ato-cli/core/src/router.rs`
> - Universal Runtime: `apps/nacelle/src/launcher/` (Toolchain Provider 実装)
> - Runtime Selection Order: `2026-01-29_000001_runtime-selection-order.md` (ADR)

## 1. 結論（責務境界）

- **ビルド時（ato-cli）**:
  - 検証（L1/L2/L3/L4 相当）
  - ランタイム解決・固定（可能な範囲で決定性を上げる）
  - 実行定義（サービス、環境、ポート、サンドボックスルール）を確定

- **実行時（nacelle）**:
  - "確定済みの定義" を読み、愚直に展開・隔離・起動・監視する

## 2. Universal Runtime（Toolchain Provider）

目的: `targets.source` をホスト依存なく動かす（python/node/bun/deno）。

- `nacelle` 内の Toolchain Provider が
  - 取得 → 検証 → 展開 → キャッシュ
  を行い、実行可能な `bin_path` を返す。
- 取得物は `~/.capsuled/toolchain` に統一し、ロックとメタデータ管理を行う。
- 検証戦略:
  - sha256（+ 将来的に署名検証/鍵固定）
  - 供給元（Node/Bun/python-build-standalone等）ごとに戦略を切替

## 3. Build Hooks（後続・用途限定）

原則: 任意コード実行に近いので **後続フェーズで限定導入**。

- Phase 1（現行）: 依存関係は基本 **pre-pack 側で解決**（vendoring / bundling）
- Phase 2（後続）: sandboxed hooks を導入する場合でも
  - deny-by-default の sandbox
  - lockfile/hashes による再現性
  - 監査ログ（入力/出力digest、コマンド、ネットワーク方針）
  を必須とする

## 4. 開発/配布（Hybrid）

- **開発**: JIT provisioning（足りないランタイムはダウンロードしてキャッシュ）
- **配布**: `.capsule` 単一ファイルとして配布し、ストリーミング検証しつつ展開・実行する

注: 旧「自己展開型単一実行ファイル」構想は存在するが、現行の配布物仕様は `CAPSULE_FORMAT_V2.md` を正とする。
