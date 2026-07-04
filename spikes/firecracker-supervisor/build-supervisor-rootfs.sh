#!/usr/bin/env bash
# Build a SUPERVISOR-mode rootfs.ext4 for the Firecracker supervisor spike.
#
# The rootfs boots the guest-agent AS the init supervisor (vsock mode). The agent
# owns the workload (app.py) and only starts it — with the env composed from the
# delivered bindings — once the session is bound-ready. This is the rootfs shape
# the v1.2 rootfs_builder emits (proven here on hardware before the Rust wiring).
#
# Usage: build-supervisor-rootfs.sh <agent-bin> <app.py> <out.ext4> [size_mib]
set -euo pipefail
AGENT="${1:?agent binary}"
APP="${2:?app.py}"
OUT="${3:?out.ext4}"
SIZE="${4:-512}"
HERE="$(cd "$(dirname "$0")" && pwd)"

BUILD=$(mktemp -d)
MNT=$(mktemp -d)
CID=""
cleanup() {
  [ -n "$CID" ] && docker rm -f "$CID" >/dev/null 2>&1 || true
  mountpoint -q "$MNT" 2>/dev/null && { sudo umount "$MNT" 2>/dev/null || sudo umount -l "$MNT" 2>/dev/null || true; }
  rm -rf "$BUILD" "$MNT" 2>/dev/null || true
}
trap cleanup EXIT

# python base filesystem via docker export (the builder uses python:3.11-slim).
docker pull -q python:3.11-slim >/dev/null
CID=$(docker create python:3.11-slim)
mkdir -p "$BUILD/rootfs"
docker export "$CID" | tar -x -C "$BUILD/rootfs"
docker rm -f "$CID" >/dev/null; CID=""

R="$BUILD/rootfs"
mkdir -p "$R/app" "$R/usr/local/bin" "$R/etc/ato" "$R/run/ato/bindings"
cp "$APP" "$R/app/app.py"
cp "$AGENT" "$R/usr/local/bin/ato-guest-agent"
chmod 0755 "$R/usr/local/bin/ato-guest-agent"

# /etc/ato/supervisor.json — the v1.2 supervisor config. NO secret value: only the
# ENV_VAR -> binding NAME map. The agent reads /run/ato/bindings/openai at spawn.
cat > "$R/etc/ato/supervisor.json" <<'JSON'
{
  "cmd": ["python3", "/app/app.py"],
  "cwd": "/app",
  "base_env": { "PORT": "8080" },
  "bindings_env": { "OPENAI_API_KEY": "openai" }
}
JSON

# init runs the guest-agent AS THE SUPERVISOR (vsock), not the app directly. The
# agent starts app.py only after the `openai` binding is delivered (bound-ready).
rm -f "$R/sbin/init"
cat > "$R/sbin/init" <<'INIT'
#!/bin/sh
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export PYTHONDONTWRITEBYTECODE=1 HOME=/tmp
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
mount -t tmpfs tmpfs /tmp 2>/dev/null
mount -t tmpfs tmpfs /run 2>/dev/null
mkdir -p /run/ato/bindings
mount -t tmpfs tmpfs /var/tmp 2>/dev/null
# The agent is the supervisor: vsock control plane on 1025, required binding "openai".
export ATO_GUEST_AGENT_MODE=vsock ATO_GUEST_AGENT_VSOCK_PORT=1025 ATO_BINDINGS_ROOT=/run/ato/bindings
/usr/local/bin/ato-guest-agent openai >/tmp/agent.log 2>&1
# If the agent exits, keep PID 1 alive so the VM does not panic-reboot.
while true; do sleep 1000; done
INIT
chmod +x "$R/sbin/init"

rm -f "$OUT"
dd if=/dev/zero of="$OUT" bs=1M count="$SIZE" status=none
mkfs.ext4 -q -F "$OUT"
sudo mount -o loop "$OUT" "$MNT"
sudo cp -a "$R/." "$MNT/"
sync
sudo umount "$MNT"
echo "### SUPERVISOR-ROOTFS $OUT ($(du -h "$OUT" | cut -f1))"
