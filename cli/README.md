# nacelle CLI (engine-facing)

このディレクトリは **nacelle のCLIバイナリ**（エンジン向けエントリポイント）を提供します。

- このバイナリは `nacelle internal ...` のような **機械向けインターフェース**（JSON over stdio）と、
  v0.2.0の **self-extracting bundle 実行**を主に扱います。
- ユーザー向けの上位CLI（`capsule`）は別レイヤ（別リポジトリ/別パッケージ）として扱う想定です。

## Build

リポジトリルートから:

```bash
cargo build -p nacelle-cli --bin nacelle
cargo build -p nacelle-cli --release --bin nacelle
```

または `cli/` で:

```bash
cd cli
cargo build --release
```

## Notes

- `cargo build` をワークスペースルートで実行すると **ライブラリ（`nacelle`）** がビルドされます。
- エンジン実行・bundle生成/実行の入口は **`nacelle-cli` の `nacelle` バイナリ**です。
