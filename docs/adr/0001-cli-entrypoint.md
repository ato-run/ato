# ADR 0001: `nacelle-cli` を唯一の `nacelle` エントリポイントにする

- Status: Accepted
- Date: 2026-01-15

## Context

このリポジトリはワークスペースルートに `nacelle`（library + package）を持ち、さらに `cli/` に `nacelle-cli`（package）を持ちます。

過去状態では以下の問題がありました:

- ワークスペースルートに `src/main.rs` が存在し、Cargoの自動検出により **ルート側にも `nacelle` バイナリが生成**され得る
- `cli/` 側も `[[bin]] name = "nacelle"` を提供しており、結果として **同名バイナリが複数起点**になり、
  - `cargo build` の実行場所で挙動が変わる
  - `target/.../nacelle` の生成物が上書きされ得る
  - ドキュメント/スクリプトがどのバイナリを前提にしているか不明瞭

## Decision

- ワークスペースルートの `src/main.rs` を削除し、ルートは **library-only** とする
- エンジン実行・bundle生成/実行の入口は **`nacelle-cli` の `nacelle` バイナリ**に一本化する
- 既存の daemon 前提の E2E（シェルスクリプト）や古い CLI ドキュメントは `docs/legacy/` / `scripts/legacy/` に退避する

## Consequences

- ワークスペースルートでのビルドは原則以下:
  - バイナリ: `cargo build -p nacelle-cli --bin nacelle`
  - ライブラリ: `cargo build -p nacelle`
- daemon 前提のレガシーE2Eは `RUN_LEGACY_DAEMON_E2E=1` のときのみ実行（pre-push）

## Diagram

```mermaid
flowchart TD
  subgraph Workspace[Cargo Workspace]
    A[nacelle (lib)]
    B[nacelle-cli (bin: nacelle)]
    C[nacelle-ebpf (bin: nacelle-ebpf)]
  end

  B -->|depends on| A

  subgraph UserFlows[主要フロー]
    U1[cargo build -p nacelle-cli --bin nacelle]
    U2[cargo build -p nacelle]
    U3[./nacelle-bundle (self-extracting)]
  end

  U1 --> B
  U2 --> A
  U3 --> B
```
