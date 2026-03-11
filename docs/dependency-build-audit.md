# Dependency and Build Audit (ato-cli)

最終更新: 2026-02-25

## Scope

- 対象: `apps/ato-cli`（`core` を含む）
- 目的: 不要依存の削減、不要ビルドの抑制、CI実行時間の最適化

## Keep (残す依存/ビルド)

- `capsule-core` 側 `bollard`
  - 根拠: OCI 実行は `core/src/runtime/oci.rs` および `core/src/executors/oci.rs` で使用。
- `tsnet` 実装と生成済み `core/src/tsnet/tsnet.v1.rs`
  - 根拠: `src/common/sidecar.rs` から `capsule_core::tsnet` を利用。
- `wasmtime` / `wasmtime-wasi` / `wasi-common`
  - 根拠: `src/commands/guest.rs` で guest wasm 実行に使用。

## Remove (削る依存/ビルド)

- `core/build.rs` と `core` の `build-dependencies.tonic-build`
  - 理由: 毎回 `protoc` を要求するため、通常ビルド要件としては過剰。
  - 代替: `core/src/tsnet/tsnet.v1.rs` をビルド入力として固定し、再生成は手動運用。
- CLI 側 `src/executors/oci.rs`
  - 理由: `src/executors/mod.rs` で公開されているだけで到達経路がない。
- CLI 側 `Cargo.toml` の `bollard`
  - 理由: `src/executors/oci.rs` 削除後、CLI 本体からの直接利用がなくなる。

## Watch (要観察)

- `core/src/tsnet/tsnet.v1.rs` の更新フロー
  - `core/proto/tsnet/v1/tsnet.proto` 変更時に手動再生成が必要。
- 警告の多い箇所
  - `src/env.rs`, `src/commands/logs.rs`, `src/registry.rs`, `src/ipc/validate.rs` を優先して整理。

## Manual proto regeneration

```bash
cd apps/ato-cli
./core/scripts/gen_tsnet_proto.sh
```

補足:
- 再生成時のみ `protoc` が必要。
- 通常の `cargo build -p ato-cli` では `protoc` 非必須。
