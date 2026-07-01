#!/usr/bin/env bash
# L1 (#912): Phase 8 (BindingLease) Linux regression harness.
#
# ONE command re-runs the Phase 8a-HW + RunGate KVM tests in a fixed form and writes a
# JSON receipt (host metadata + per-test pass/fail + orphan check). Run on a KVM host.
#
#   ATO_FC_BIN=~/bin/firecracker ATO_FC_KERNEL=~/bin/vmlinux \
#   ATO_FC_BINDING_ROOTFS=~/bench/binding-app.ext4 \
#   scripts/ready-state/phase8-regression.sh [out_dir]
#
# The binding rootfs must serve /ready (always 200) + /health (200 only when
# /run/ato/bindings/api_key exists) and launch ato-guest-agent in vsock mode requiring
# `api_key` (see docs/ready-state/guest-agent-packaging.md). Requires: sudo, jq.
set -uo pipefail

: "${ATO_FC_BIN:?set ATO_FC_BIN}"; : "${ATO_FC_KERNEL:?set ATO_FC_KERNEL}"
: "${ATO_FC_BINDING_ROOTFS:?set ATO_FC_BINDING_ROOTFS to a binding-required rootfs}"
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="${1:-$REPO/artifacts/ready-state/phase8-regression/$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p "$OUT"
WORK="${ATO_FC_WORK:-/tmp/ato-fc-p8reg}"

# ── host metadata ──────────────────────────────────────────────────────────────
sudo modprobe vhost_vsock 2>/dev/null || true
VHOST=$(lsmod 2>/dev/null | grep -qw vhost_vsock && echo true || echo false)
KVM=$([ -e /dev/kvm ] && echo true || echo false)
FCV=$("$ATO_FC_BIN" --version 2>/dev/null | head -1 | awk '{print $NF}')
GK=$(basename "$ATO_FC_KERNEL")
ROOTFS_ID="blake3:$(b3sum "$ATO_FC_BINDING_ROOTFS" 2>/dev/null | awk '{print $1}' || sha256sum "$ATO_FC_BINDING_ROOTFS" | awk '{print $1}')"

run_test() {
  local name="$1"
  sudo pkill -9 firecracker 2>/dev/null; sudo ip link del fctap0 2>/dev/null; sudo rm -rf "$WORK" /tmp/ato-vsock; sleep 2
  local log="$OUT/$name.log"
  sudo -E env PATH="$PATH" ATO_FC_BIN="$ATO_FC_BIN" ATO_FC_KERNEL="$ATO_FC_KERNEL" \
    ATO_FC_BINDING_ROOTFS="$ATO_FC_BINDING_ROOTFS" ATO_FC_ROOTFS_READONLY=1 ATO_FC_WORK="$WORK" ATO_FC_BOOT_TIMEOUT_S=90 \
    cargo test -p snapshot --release "$name" -- --ignored --test-threads=1 --nocapture >"$log" 2>&1
  grep -qaE "test result: ok\. 1 passed" "$log" && echo pass || echo fail
}

echo "[phase8-regression] live E2E …";      E2E=$(run_test fc_kvm_binding_lease_live_e2e)
echo "[phase8-regression] negative paths …"; NEG=$(run_test fc_kvm_binding_negative_paths)

# ── orphan check (after the runs) ──────────────────────────────────────────────
sudo pkill -9 firecracker 2>/dev/null; sleep 1
ORPH_FC=$(pgrep -af "firecracker --api-sock" 2>/dev/null | grep -c . || echo 0)
ORPH_TAP=$(ip link show fctap0 >/dev/null 2>&1 && echo 1 || echo 0)
ORPH_VSOCK=$(ls /tmp/ato-vsock/*.sock 2>/dev/null | grep -c . || echo 0)
ORPH_OVERLAY=$(ls -d "$WORK" 2>/dev/null | grep -c . || echo 0)

PASS=$([ "$E2E" = pass ] && [ "$NEG" = pass ] && [ "$ORPH_FC" = 0 ] && [ "$ORPH_TAP" = 0 ] && [ "$ORPH_VSOCK" = 0 ] && echo true || echo false)

jq -n --arg e2e "$E2E" --arg neg "$NEG" --arg fcv "$FCV" --arg gk "$GK" --arg rootfs "$ROOTFS_ID" \
  --argjson kvm "$KVM" --argjson vhost "$VHOST" --arg kernel "$(uname -r)" \
  --argjson ofc "$ORPH_FC" --argjson otap "$ORPH_TAP" --argjson ovs "$ORPH_VSOCK" --argjson oov "$ORPH_OVERLAY" \
  --argjson pass "$PASS" '{
    phase: "phase8_regression",
    host: { kernel: $kernel, firecracker_version: $fcv, guest_kernel: $gk, rootfs_id: $rootfs, kvm: $kvm, vhost_vsock: $vhost },
    tests: { fc_kvm_binding_lease_live_e2e: $e2e, fc_kvm_binding_negative_paths: $neg },
    orphans: { firecracker: $ofc, tap: $otap, vsock: $ovs, overlay: $oov },
    pass: $pass
  }' | tee "$OUT/results.json"
echo "### PHASE8-REGRESSION $( [ "$PASS" = true ] && echo PASS || echo FAIL ) → $OUT/results.json"
[ "$PASS" = true ]
