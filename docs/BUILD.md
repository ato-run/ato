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

---

## eBPF Build (Linux Only)

eBPF プログラムは Linux カーネル内で動作する低レベルコード。ビルドには特定のツール要件があります。

### 前提条件（Linux のみ）

eBPF ビルドはターゲットが Linux の場合のみ有効化されます。macOS や Windows では **eBPF ビルドステップはスキップされます**。

**Linux 環境で必要なツール:**

```bash
# Ubuntu/Debian
sudo apt-get update
sudo apt-get install -y \
    llvm-14 \
    clang-14 \
    libelf-dev \
    libz-dev \
    pkg-config \
    linux-headers-$(uname -r)

# Fedora/RHEL
sudo dnf install -y \
    llvm-devel \
    clang-devel \
    elfutils-libelf-devel \
    zlib-devel \
    kernel-devel

# Arch Linux
sudo pacman -S llvm clang linux-headers

# macOS (開発用 / eBPF は実行されません)
brew install llvm@14
```

### eBPF ビルドプロセス

`build.rs` スクリプトが Linux 環境を検出すると、自動的に eBPF プログラムをビルドします：

1. Rust nightly toolchain を使用して eBPF bytecode (`nacelle-ebpf`) をコンパイル
2. BTF（BPF Type Format）デバッグ情報を埋め込み
3. ELF オブジェクトファイルを生成して `target/debug/build/nacelle-*/out/nacelle-ebpf` に配置

**ビルド実行:**

```bash
# Linux 環境で通常のビルドを実行
cargo build --lib

# eBPF プログラムのみをビルド
cd ebpf
rustup toolchain install nightly
rustup component add rust-src --toolchain nightly
cargo +nightly build -Z build-std=core --target bpfeb-unknown-none --release
```

### トラブルシューティング（eBPF）

#### `llvm-config` が見つからない

```bash
# LLVM_CONFIG 環境変数を明示的に設定
export LLVM_CONFIG=/usr/bin/llvm-config-14
cargo build --lib
```

#### `clang` が見つからない

```bash
# eBPF は clang をコンパイラとして使用
export CLANG=/usr/bin/clang-14
cargo build --lib
```

#### 権限エラー（`bpftool` が必要）

BTF デバッグ情報の検証に `bpftool` が必要な場合：

```bash
# Ubuntu/Debian
sudo apt-get install linux-tools-generic

# Fedora
sudo dnf install kernel-tools

# Arch
sudo pacman -S bpf
```

---

## Protocol Buffer (Protobuf) Code Generation

Protocol Buffers は言語中立なシリアライゼーション形式で、gRPC サービス定義に使用されます。

### 前提条件

```bash
# protoc コンパイラをインストール
# Ubuntu/Debian
sudo apt-get install protobuf-compiler

# macOS
brew install protobuf

# Fedora
sudo dnf install protobuf-compiler

# Verify installation
protoc --version
```

### Proto ファイル

Proto ファイルはリポジトリ内の `proto/` ディレクトリに配置されています：

- `proto/common/v1/common.proto` - 共通メッセージ型（RunPlan など）
- `proto/engine/v1/api.proto` - Engine サービス定義

### ビルドプロセス

`build.rs` スクリプトが自動的に以下を実行します：

1. `tonic_build::compile_protos()` で `.proto` ファイルをコンパイル
2. Rust コード生成（`src/proto/nacelle.*.rs`）
3. gRPC スタブ生成

**手動再生成:**

```bash
# キャッシュをクリア
cargo clean

# ビルド時に自動再生成
cargo build
```

### Proto パッケージ名

現在のパッケージ名は `nacelle.*` で統一されています：

- `nacelle.common.v1` - 共通型
- `nacelle.engine.v1` - Engine サービス

**Rust 内でのアクセス:**

```rust
use crate::proto::nacelle::engine::v1::{DeployRequest, DeployResponse};
```

### proto ファイルの修正時の注意

`.proto` ファイルを編集した場合：

1. パッケージ名は `nacelle.*` を使用（`onescluster` は使用不可）
2. Go/Java オプションを追加する場合は慎重に（他言語との互換性に影響）
3. ビルド後、生成ファイル（`src/proto/nacelle.*.rs`）は **自動生成** のため手修正は避ける
4. CI で proto 生成の再現性を確認（`.proto` 変更時は必ず `cargo clean && cargo build`）

### トラブルシューティング（Proto）

#### `protoc` が見つからない

```bash
# PROTOC 環境変数を設定
export PROTOC=/usr/bin/protoc
cargo build
```

#### proto コンパイルエラー

```bash
# verbose でビルド
RUST_LOG=debug cargo build 2>&1 | grep -i proto
```

#### 生成ファイル更新が反映されない

```bash
# キャッシュをクリアして再生成
cargo clean
cargo build --lib
```

---

## Complete Development Build (All Targets)

すべての機能を含む完全なビルド：

```bash
# 1. 前提条件のインストール
./scripts/setup-build-env.sh

# 2. フォーマットチェック
cargo fmt --check

# 3. Lint 実行
cargo clippy --all-targets -- -D warnings

# 4. テスト実行
cargo test --lib

# 5. ドキュメント生成
cargo doc --no-deps

# 6. リリースビルド
cargo build --release

# 7. CLI ビルド（オプション）
cargo install --path ./cli
```

### 参考資料

- [Rust Cross Compilation Guide](https://rust-lang.github.io/rustup/cross-compilation.html)
- [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild)
- [cross](https://github.com/cross-rs/cross)
- [eBPF (Linux Kernel Documentation)](https://ebpf.io)
- [Protocol Buffers](https://developers.google.com/protocol-buffers)
- [tonic gRPC (Rust)](https://github.com/hyperium/tonic)
