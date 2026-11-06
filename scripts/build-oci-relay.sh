#!/bin/sh
# Build the OCI relay binaries that `ato-adapter-oci` embeds, and record their
# hashes.
#
# The relay runs inside the Run's container network, so it must be a static
# Linux binary. Both architectures Docker can report are shipped, because the
# relay is chosen by the daemon's architecture, not by the CLI's.
#
# The build is pinned so that CI can reproduce it and compare the result with
# the committed binaries (extensions/adapters/oci/relay-bin/SHA256SUMS):
#   * the toolchain is the one `rust-toolchain.toml` pins,
#   * zig is an exact version,
#   * the checkout and registry paths are remapped, so the bytes do not carry
#     the directory this build happened to run in.
# A different zig or rustc may still produce a different (but equivalent)
# binary, which is why both are checked rather than assumed.
#
# Updating the committed binaries: run this script on the CI runner image (the
# `relay-binaries` job uploads what it built as the `relay-binaries` artifact),
# copy the artifact over extensions/adapters/oci/relay-bin/, and commit it with
# the source change. The job then compares the committed bytes with a fresh
# rebuild and fails if they differ.
#
# Requirements: zig 0.15.2 on PATH, and the musl targets for the pinned
# toolchain:
#   rustup target add --toolchain 1.96.0 aarch64-unknown-linux-musl x86_64-unknown-linux-musl
#
# Run from the repository root. Set ATO_RELAY_ZIG to override the zig binary.
set -eu

PINNED_ZIG=0.15.2
PINNED_TOOLCHAIN=1.96.0

root=$(cd "$(dirname "$0")/.." && pwd)
zig=${ATO_RELAY_ZIG:-zig}

have_zig=$("$zig" version 2>/dev/null || true)
if [ "$have_zig" != "$PINNED_ZIG" ]; then
  echo "build-oci-relay: zig $PINNED_ZIG is required, found ${have_zig:-none}" >&2
  echo "build-oci-relay: install it or set ATO_RELAY_ZIG" >&2
  exit 1
fi

if ! rustup target list --installed --toolchain "$PINNED_TOOLCHAIN" 2>/dev/null | grep -q aarch64-unknown-linux-musl; then
  echo "build-oci-relay: run 'rustup target add --toolchain $PINNED_TOOLCHAIN aarch64-unknown-linux-musl x86_64-unknown-linux-musl'" >&2
  exit 1
fi

work="$root/.tmp/oci-relay-build"
mkdir -p "$work/tools" "$work/zig-cache" "$work/zig-local"

cat > "$work/tools/zig-cc-arm64.sh" <<INNER
#!/bin/sh
exec "$zig" cc -target aarch64-linux-musl "\$@"
INNER
cat > "$work/tools/zig-cc-amd64.sh" <<INNER
#!/bin/sh
exec "$zig" cc -target x86_64-linux-musl "\$@"
INNER
chmod +x "$work/tools/zig-cc-"*.sh

export ZIG_GLOBAL_CACHE_DIR="$work/zig-cache"
export ZIG_LOCAL_CACHE_DIR="$work/zig-local"
export CARGO_TARGET_DIR="$work/target"
# The musl target ships its own crt objects; zig provides them too.
export RUSTFLAGS="-C link-self-contained=no \
  --remap-path-prefix=$root=/src/ato \
  --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}/registry=/cargo/registry"

CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="$work/tools/zig-cc-arm64.sh" \
  cargo "+$PINNED_TOOLCHAIN" build -p ato-adapter-oci --bin ato-oci-relay --release \
    --target aarch64-unknown-linux-musl --locked
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$work/tools/zig-cc-amd64.sh" \
  cargo "+$PINNED_TOOLCHAIN" build -p ato-adapter-oci --bin ato-oci-relay --release \
    --target x86_64-unknown-linux-musl --locked

bin="$root/extensions/adapters/oci/relay-bin"
mkdir -p "$bin/linux-arm64" "$bin/linux-amd64"
cp "$work/target/aarch64-unknown-linux-musl/release/ato-oci-relay" "$bin/linux-arm64/ato-oci-relay"
cp "$work/target/x86_64-unknown-linux-musl/release/ato-oci-relay" "$bin/linux-amd64/ato-oci-relay"
chmod 755 "$bin/linux-arm64/ato-oci-relay" "$bin/linux-amd64/ato-oci-relay"

cd "$bin"
shasum -a 256 linux-arm64/ato-oci-relay linux-amd64/ato-oci-relay > SHA256SUMS
cat SHA256SUMS
