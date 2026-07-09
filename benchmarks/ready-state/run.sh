#!/usr/bin/env bash
# Thin wrapper to run the Ready-State latency benchmark for ONE rootfs/target on a
# KVM host. Build the app-class rootfs images separately and pass each via ROOTFS.
#
# Usage:
#   ATO_FC_BIN=.../firecracker ATO_FC_KERNEL=.../vmlinux \
#   ROOTFS=.../rootfs.ext4 TARGET=tiny-http ./benchmarks/ready-state/run.sh
#
# Env (with defaults):
#   ATO_FC_ROOTFS_READONLY=0   # validated fresh-copy mode (1 = ro-shared, unvalidated)
#   ATO_FC_WORK=/tmp/ato-fc-bench
#   BUILD_RUNS=5  RESTORE_RUNS=30
set -euo pipefail

: "${ATO_FC_BIN:?set ATO_FC_BIN to the firecracker binary}"
: "${ATO_FC_KERNEL:?set ATO_FC_KERNEL to the guest kernel}"
: "${ROOTFS:?set ROOTFS to the app rootfs.ext4}"
TARGET="${TARGET:-unnamed}"
ATO_FC_ROOTFS_READONLY="${ATO_FC_ROOTFS_READONLY:-0}"
ATO_FC_WORK="${ATO_FC_WORK:-/tmp/ato-fc-bench}"
BUILD_RUNS="${BUILD_RUNS:-5}"
RESTORE_RUNS="${RESTORE_RUNS:-30}"
OUT="${OUT:-benchmarks/ready-state}"

# Stale-resource hygiene (single-session backend).
sudo pkill -9 firecracker 2>/dev/null || true
sudo ip link del fctap0 2>/dev/null || true
sudo rm -rf "$ATO_FC_WORK"

sudo -E env \
  ATO_READY_STATE_BENCH=1 \
  ATO_FC_BIN="$ATO_FC_BIN" \
  ATO_FC_KERNEL="$ATO_FC_KERNEL" \
  ATO_FC_ROOTFS_READONLY="$ATO_FC_ROOTFS_READONLY" \
  ATO_FC_WORK="$ATO_FC_WORK" \
  cargo run -p snapshot --release --bin ready-state-bench -- \
    --rootfs "$ROOTFS" --target "$TARGET" \
    --build-runs "$BUILD_RUNS" --restore-runs "$RESTORE_RUNS" \
    --out "$OUT"
