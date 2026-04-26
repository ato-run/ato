#!/bin/bash
# Setup cross-compilation build environment on macOS

set -e

echo "🔧 Setting up cross-compilation environment"
echo "==========================================="

# Check if Homebrew is installed
if ! command -v brew &> /dev/null; then
    echo "❌ Homebrew is not installed. Please install it first:"
    echo "   /bin/bash -c \"\$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\""
    exit 1
fi

# Install Zig (for cargo-zigbuild)
echo "📦 Installing Zig..."
brew install zig

# Install MinGW-w64 (for Windows targets)
echo "📦 Installing MinGW-w64..."
brew install mingw-w64

# Install Protocol Buffers compiler (required for build.rs)
echo "📦 Installing protobuf..."
brew install protobuf

# Install Cap'n Proto (required for build.rs)
echo "📦 Installing Cap'n Proto..."
brew install capnp

# Install cargo-zigbuild
echo "📦 Installing cargo-zigbuild..."
cargo install cargo-zigbuild

# Add required Rust targets
echo "📋 Adding Rust targets..."
rustup target add x86_64-unknown-linux-musl
rustup target add aarch64-unknown-linux-musl
rustup target add x86_64-pc-windows-gnu
rustup target add aarch64-apple-darwin
rustup target add x86_64-apple-darwin

echo ""
echo "✅ Setup complete!"
echo ""
echo "You can now run: ./scripts/build-release.sh"
