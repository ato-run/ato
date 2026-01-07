#!/bin/bash
# Cross-platform release build script for capsuled
# Builds binaries for macOS, Linux (musl), and Windows from macOS

set -e

echo "🚀 Cross-platform Release Build"
echo "================================"

# Check if cargo-zigbuild is installed
if ! command -v cargo-zigbuild &> /dev/null; then
    echo "📦 Installing cargo-zigbuild..."
    cargo install cargo-zigbuild
fi

# Add required targets
echo "📋 Adding build targets..."
rustup target add x86_64-unknown-linux-musl
rustup target add aarch64-unknown-linux-musl
rustup target add x86_64-pc-windows-gnu
rustup target add aarch64-apple-darwin
rustup target add x86_64-apple-darwin

# Create release directory
RELEASE_DIR="./release"
rm -rf "$RELEASE_DIR"
mkdir -p "$RELEASE_DIR"

echo ""
echo "🍎 Building for macOS (Universal Binary)..."
cargo build --release --target x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin
lipo -create \
    target/x86_64-apple-darwin/release/capsuled \
    target/aarch64-apple-darwin/release/capsuled \
    -output "$RELEASE_DIR/capsuled-macos-universal"
echo "✅ macOS universal binary: $RELEASE_DIR/capsuled-macos-universal"

echo ""
echo "🐧 Building for Linux x86_64 (musl, static)..."
cargo zigbuild --release --target x86_64-unknown-linux-musl
cp target/x86_64-unknown-linux-musl/release/capsuled "$RELEASE_DIR/capsuled-linux-x86_64"
echo "✅ Linux x86_64: $RELEASE_DIR/capsuled-linux-x86_64"

echo ""
echo "🐧 Building for Linux aarch64 (musl, static)..."
cargo zigbuild --release --target aarch64-unknown-linux-musl
cp target/aarch64-unknown-linux-musl/release/capsuled "$RELEASE_DIR/capsuled-linux-aarch64"
echo "✅ Linux aarch64: $RELEASE_DIR/capsuled-linux-aarch64"

echo ""
echo "🪟 Building for Windows x86_64..."
# Install mingw-w64 if not present
if ! command -v x86_64-w64-mingw32-gcc &> /dev/null; then
    echo "📦 Installing mingw-w64..."
    brew install mingw-w64
fi
cargo build --release --target x86_64-pc-windows-gnu
cp target/x86_64-pc-windows-gnu/release/capsuled.exe "$RELEASE_DIR/capsuled-windows-x86_64.exe"
echo "✅ Windows x86_64: $RELEASE_DIR/capsuled-windows-x86_64.exe"

echo ""
echo "✨ Build complete! Binaries in: $RELEASE_DIR/"
ls -lh "$RELEASE_DIR/"
