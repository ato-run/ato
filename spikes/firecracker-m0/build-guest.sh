#!/usr/bin/env bash
# KVM-FREE: build the guest kernel + minimal rootfs + bake the hello web app.
# Produces ${GUEST_KERNEL} and ${ROOTFS} consumed by run-spike.sh. Safe to run on the
# A1 box without /dev/kvm (it only builds artifacts; it does not boot anything).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"; cd "${here}"
# shellcheck disable=SC1091
. ./versions.env

echo "==> 1. Fetch pinned Firecracker ${FC_VERSION} (${FC_ARCH})"
# TODO: download the firecracker + jailer binaries for FC_ARCH from the pinned release,
#       verify the checksum, place ./firecracker and ./jailer here, chmod +x.

echo "==> 2. Guest kernel (vmlinux)"
# TODO: fetch GUEST_KERNEL_URL or build a minimal vmlinux ${GUEST_KERNEL} matching
#       Firecracker's supported config for ${FC_ARCH}.

echo "==> 3. Minimal rootfs (Alpine + tiny init + hello app on :${GUEST_PORT}${HEALTH_PATH})"
# Build an ext4 rootfs: Alpine base, a PID1 init that (a) brings up eth0, (b) starts the
# hello web app, (c) mounts a vsock control channel for the guest-agent (see protocol md).
# The hello app must expose ${HEALTH_PATH} returning 200 with NO secret required, plus a
# /marker endpoint used by leak-test.sh (writes an in-memory marker; GET returns it).
#
#   apk add --no-cache busybox
#   cat > /sbin/hello-init <<'INIT' ... INIT   # brings up net, starts the app
#   # app pseudocode:
#   #   GET /health -> 200 OK            (secret-free readiness point = the seal point)
#   #   POST /marker {v} -> store v in process memory
#   #   GET  /marker -> last stored v (empty on a fresh boot)
#
# Output: ${ROOTFS}

echo "build-guest.sh: fill the TODOs on the chosen host, then run ./run-spike.sh"
