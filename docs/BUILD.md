# Building nacelle

## Cross-Platform Release Build (macOS)

nacelleは、macOS上からすべてのプラットフォーム向けのバイナリをビルドできます。

### 前提条件

- macOS (Apple Silicon or Intel)
- Rust toolchain (rustup)
- Homebrew

### クイックスタート

```bash
# 1回目のみ：依存ツールのインストール
./scripts/setup-build-env.sh

# リリースビルド実行
./scripts/build-release.sh
```

生成されたバイナリは `./release/` ディレクトリに配置されます：

- `nacelle-macos-universal` - macOS Universal Binary (x86_64 + arm64)
- `nacelle-linux-x86_64` - Linux x86_64 (静的リンク、musl)
- `nacelle-linux-aarch64` - Linux ARM64 (静的リンク、musl)
- `nacelle-windows-x86_64.exe` - Windows x86_64 (MinGW)

### 使用ツール

#### cargo-zigbuild（推奨）

2026年時点で最も推奨されるクロスコンパイルツール。Zigのlinkerを使用してC依存関係の問題を回避します。

**メリット:**
- セットアップが最小限
- Docker不要
- 静的リンクが容易（musl）
- glibc最小バージョンの指定が可能

**制限:**
- Windows MSVCターゲットは非対応（GNU/MinGWのみ）

#### cross（代替案）

C/C++依存が複雑な場合の代替ツール。Docker環境で完全に隔離してビルドします。

```bash
# crossを使う場合
cargo install cross
cross build --release --target x86_64-unknown-linux-musl
cross build --release --target x86_64-pc-windows-gnu
```

### 個別プラットフォームのビルド

#### macOS（ネイティブ）

```bash
# Intel
cargo build --release --target x86_64-apple-darwin

# Apple Silicon
cargo build --release --target aarch64-apple-darwin

# Universal Binary
lipo -create \
    target/x86_64-apple-darwin/release/nacelle \
    target/aarch64-apple-darwin/release/nacelle \
    -output nacelle-universal
```

#### Linux (musl - 静的リンク推奨)

```bash
# x86_64
cargo zigbuild --release --target x86_64-unknown-linux-musl

# aarch64
cargo zigbuild --release --target aarch64-unknown-linux-musl
```

#### Windows

```bash
# GNU/MinGW (推奨)
cargo build --release --target x86_64-pc-windows-gnu

# MSVC (cargo-xwin必要)
cargo install cargo-xwin
cargo xwin build --release --target x86_64-pc-windows-msvc
```

### トラブルシューティング

#### Zigが見つからない

```bash
brew install zig
cargo install cargo-zigbuild
```

#### MinGW-w64が見つからない

```bash
brew install mingw-w64
```

#### リンカーエラー (Linux)

muslターゲットを使用してください：
```bash
rustup target add x86_64-unknown-linux-musl
cargo zigbuild --release --target x86_64-unknown-linux-musl
```

#### バイナリサイズの削減

```bash
# strip使用
strip target/release/nacelle

# Cargo.tomlに追加
[profile.release]
strip = true
opt-level = "z"
lto = true
codegen-units = 1
```

### CI/CDからの移行

以前はGitHub Actionsを使用していましたが、以下の理由でローカルビルドに移行しました：

- macOS runnerの実行時間削減
- ビルド環境の完全な制御
- cargo-zigbuildによる効率的なクロスコンパイル

### 参考資料

- [Rust Cross Compilation Guide](https://rust-lang.github.io/rustup/cross-compilation.html)
- [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild)
- [cross](https://github.com/cross-rs/cross)
