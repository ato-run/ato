#!/bin/bash
# Cross-platform release build script for nacelle
# Builds binaries for macOS, Linux (musl), and Windows from macOS

set -e

# Change to project root directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.."

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
cargo build --release --target x86_64-apple-darwin -p nacelle-cli --bin nacelle
cargo build --release --target aarch64-apple-darwin -p nacelle-cli --bin nacelle
lipo -create \
    target/x86_64-apple-darwin/release/nacelle \
    target/aarch64-apple-darwin/release/nacelle \
    -output "$RELEASE_DIR/nacelle-macos-universal"
echo "✅ macOS universal binary: $RELEASE_DIR/nacelle-macos-universal"

echo ""
echo "🐧 Building for Linux x86_64 (musl, static)..."
cargo zigbuild --release --target x86_64-unknown-linux-musl -p nacelle-cli --bin nacelle
cp target/x86_64-unknown-linux-musl/release/nacelle "$RELEASE_DIR/nacelle-linux-x86_64"
echo "✅ Linux x86_64: $RELEASE_DIR/nacelle-linux-x86_64"

echo ""
echo "🐧 Building for Linux aarch64 (musl, static)..."
cargo zigbuild --release --target aarch64-unknown-linux-musl -p nacelle-cli --bin nacelle
cp target/aarch64-unknown-linux-musl/release/nacelle "$RELEASE_DIR/nacelle-linux-aarch64"
echo "✅ Linux aarch64: $RELEASE_DIR/nacelle-linux-aarch64"

echo ""
echo "🪟 Building for Windows x86_64..."
# Install mingw-w64 if not present
if ! command -v x86_64-w64-mingw32-gcc &> /dev/null; then
    echo "📦 Installing mingw-w64..."
    brew install mingw-w64
fi
cargo build --release --target x86_64-pc-windows-gnu -p nacelle-cli --bin nacelle
cp target/x86_64-pc-windows-gnu/release/nacelle.exe "$RELEASE_DIR/nacelle-windows-x86_64.exe"
echo "✅ Windows x86_64: $RELEASE_DIR/nacelle-windows-x86_64.exe"

echo ""
echo "✨ Build complete! Binaries in: $RELEASE_DIR/"
ls -lh "$RELEASE_DIR/"
