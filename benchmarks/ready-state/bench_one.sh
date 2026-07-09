#!/usr/bin/env bash
set -uo pipefail
TARGET=$1 ROOTFS=$2 BR=${3:-5} RR=${4:-30} RO=${5:-0}
H=/home/ubuntu
sudo pkill -9 firecracker 2>/dev/null || true
sudo ip link del fctap0 2>/dev/null || true
sudo rm -rf /tmp/ato-fc-bench
echo "### BENCH $TARGET ro=$RO (build=$BR restore=$RR)"
sudo -E env ATO_READY_STATE_BENCH=1 ATO_FC_BIN=$H/bench/firecracker ATO_FC_KERNEL=$H/bench/vmlinux \
  ATO_FC_ROOTFS_READONLY=$RO ATO_FC_WORK=/tmp/ato-fc-bench ATO_FC_BOOT_TIMEOUT=40 \
  $H/ato-bench/target/release/ready-state-bench --rootfs $ROOTFS --target ${TARGET}-ro${RO} --build-runs $BR --restore-runs $RR --out $H/bench-out 2>&1 | tail -22
echo "### EXIT $TARGET ro=$RO rc=${PIPESTATUS[0]}"
