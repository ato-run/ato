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

## 3. Target-level `build` コマンド（`ato run` の build フェーズ）

`[targets.<label>].build` フィールドで宣言された shell コマンドは、`ato run` の **build フェーズ**（primary runtime のプロビジョニング完了後、`run` コマンド起動前）に実行される。これは ato-cli 自身が直接実行する。

**ライフサイクル順序**（`ato run` の build フェーズ内）:

```
1. primary runtime を解決・準備する
2. primary runtime の依存関係を materialize する
3. runtime_tools で宣言された lifecycle toolchains を build 実行前に materialize する
4. [targets.<label>].build コマンドを実行する
5. [targets.<label>].run コマンドを起動する
```

重要: **`runtime_tools` は build コマンドよりも前に PATH へ追加される**。
uv pip install と runtime_tools materialization の厳密な順序は実装依存であり、ここでは規定しない。

**混在 toolchain の典型例（Python + Node）**:

```toml
[targets.main]
runtime = "source"
driver = "python"
runtime_version = "3.11"
runtime_tools = { node = "20" }
build = "npm install && npm run build"
run = "python -m uvicorn app.main:app --host 127.0.0.1"
port = 8000
```

この構成では、Node 20 が `runtime_tools` 経由でプロビジョニングされ、`build` フェーズで `npm run build` が実行されて `dist/` が生成される。実装は `crates/ato-cli/src/application/pipeline/phases/run.rs` の `run_build_phase` および `lifecycle_path.rs` の `materialize_lifecycle_toolchains` を参照。

**旧記述について**: この文書の以前のバージョンでは「build hooks は後続・用途限定、依存関係は pre-pack 側で解決」と記載していたが、これは v0.5.x 時点の実装と乖離している。`[targets.<label>].build` は現行の `ato run` で動作する機能であり、CI/配布専用ではない。

## 4. 開発/配布（Hybrid）

- **開発**: JIT provisioning（足りないランタイムはダウンロードしてキャッシュ）
- **配布**: `.capsule` 単一ファイルとして配布し、ストリーミング検証しつつ展開・実行する

注: 旧「自己展開型単一実行ファイル」と Artifact Format v2 は
[`CAPSULE_FORMAT_V2.md`](../archived/CAPSULE_FORMAT_V2.md) にarchiveされている。
State + I/O のportable containerは
[`CAPSULE_BUNDLE_V1.md`](protocol/CAPSULE_BUNDLE_V1.md)を正本とする。
