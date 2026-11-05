#!/bin/sh
# Build the OCI relay binaries that `ato-adapter-oci` embeds.
#
# The relay runs inside the Run's container network, so it must be a static
# Linux binary. Both architectures Docker can report are shipped, because the
# relay is chosen by the daemon's architecture, not by the CLI's.
#
# Requires: zig (https://ziglang.org) on PATH, and the musl targets for the
# toolchain the workspace pins. The pinned toolchain may not ship them; install
# once with `rustup target add aarch64-unknown-linux-musl x86_64-unknown-linux-musl`.
#
# Run from the repository root.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
work="$root/.tmp/oci-relay-build"
mkdir -p "$work/tools" "$work/zig-cache" "$work/zig-local"

cat > "$work/tools/zig-cc-arm64.sh" <<'INNER'
#!/bin/sh
exec zig cc -target aarch64-linux-musl "$@"
INNER
cat > "$work/tools/zig-cc-amd64.sh" <<'INNER'
#!/bin/sh
exec zig cc -target x86_64-linux-musl "$@"
INNER
chmod +x "$work/tools/zig-cc-"*.sh

export ZIG_GLOBAL_CACHE_DIR="$work/zig-cache"
export ZIG_LOCAL_CACHE_DIR="$work/zig-local"
export CARGO_TARGET_DIR="$work/target"
# The musl target ships its own crt objects; zig provides them too.
export RUSTFLAGS="-C link-self-contained=no"

CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="$work/tools/zig-cc-arm64.sh" \
  cargo build -p ato-oci-relay --bin ato-oci-relay --release --target aarch64-unknown-linux-musl
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$work/tools/zig-cc-amd64.sh" \
  cargo build -p ato-oci-relay --bin ato-oci-relay --release --target x86_64-unknown-linux-musl

mkdir -p "$root/extensions/adapters/oci/relay-bin/linux-arm64" \
         "$root/extensions/adapters/oci/relay-bin/linux-amd64"
cp "$work/target/aarch64-unknown-linux-musl/release/ato-oci-relay" \
   "$root/extensions/adapters/oci/relay-bin/linux-arm64/ato-oci-relay"
cp "$work/target/x86_64-unknown-linux-musl/release/ato-oci-relay" \
   "$root/extensions/adapters/oci/relay-bin/linux-amd64/ato-oci-relay"
chmod 755 "$root/extensions/adapters/oci/relay-bin/"*"/ato-oci-relay"
echo "wrote relay binaries:"
ls -l "$root/extensions/adapters/oci/relay-bin/"*"/ato-oci-relay"
