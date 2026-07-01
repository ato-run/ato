#!/usr/bin/env bash
# Phase 8a-HW PR A: build the static guest-agent binary and install it into a
# Firecracker guest rootfs at /usr/local/bin/ato-guest-agent (mode 0755).
#
# The agent is a static musl binary so it runs in a minimal guest rootfs with no libc
# dependency. It runs as a foreground process in the guest; PR B selects vsock mode via
# ATO_GUEST_AGENT_MODE=vsock (stdio JSON-lines is the default + test transport).
#
#   scripts/ready-state/package-guest-agent.sh <rootfs.ext4>
#
# Requires: rustup musl target, sudo (loopback mount), e2fsprogs.
set -euo pipefail
ROOTFS="${1:?usage: package-guest-agent.sh <rootfs.ext4>}"
TARGET="${ATO_GUEST_TARGET:-x86_64-unknown-linux-musl}"
REPO="$(cd "$(dirname "$0")/../.." && pwd)"

rustup target add "$TARGET" >/dev/null 2>&1 || true
( cd "$REPO" && cargo build -p guest-agent --release --target "$TARGET" )
BIN="$REPO/target/$TARGET/release/guest-agent"
[ -x "$BIN" ] || { echo "guest-agent binary not built at $BIN" >&2; exit 1; }
# Static check: no dynamic interpreter (musl static).
if command -v file >/dev/null && file "$BIN" | grep -q "dynamically linked"; then
  echo "WARNING: guest-agent is dynamically linked; a static musl build is expected" >&2
fi

mnt="$(mktemp -d)"
cleanup() { sudo umount "$mnt" 2>/dev/null || true; rmdir "$mnt" 2>/dev/null || true; }
trap cleanup EXIT
sudo mount -o loop "$ROOTFS" "$mnt"
sudo mkdir -p "$mnt/usr/local/bin"
sudo cp "$BIN" "$mnt/usr/local/bin/ato-guest-agent"
sudo chmod 0755 "$mnt/usr/local/bin/ato-guest-agent"
sudo sync
echo "### GUEST-AGENT-PACKAGED $ROOTFS -> /usr/local/bin/ato-guest-agent"
