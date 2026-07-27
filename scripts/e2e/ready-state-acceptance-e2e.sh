#!/usr/bin/env bash
# Interactive-capture acceptance E2E, safe to run on a LIVE runner host.
#
# Drives the PRODUCTION ordering that #1155 introduced —
#   HoldPhase -> capture -> ReleasedHold -> verify_captured_candidate
#   -> disposable restore -> acceptance
# — on a real Firecracker guest, N times, recording timings.
#
# ## Why this is not `scripts/ready-state/run-uffd-kvm-smokes.sh`
#
# That script begins every test with `pkill -9 firecracker` and
# `ip link del fctap0`. On a host running `ato-runner-agent` /
# `ato-snapshot-builder` that kills production VMs and deletes the production
# builder's tap. This script kills nothing it did not start and deletes nothing
# it did not create; on ANY overlap with an existing resource it aborts instead.
#
# ## Isolation
#
# Every collision-capable resource is run-unique and prefixed `atoe2e`:
#   ATO_FC_WORK   /tmp/atoe2e-<run>/fc-work     (never /tmp/ato-fc)
#   ATO_FC_TAP    atoe2e<n>                     (never fctap0, never ato-slot-*)
#   IPs           a caller-chosen /30, pre-checked as unused
#   CAS/scratch   /tmp/atoe2e-<run>/...
#
# ## Usage
#   ATO_FC_BIN=/usr/local/bin/firecracker \
#   ATO_FC_KERNEL=/var/lib/ato/kernel/vmlinux-5.10.223 \
#   ATO_FC_TEST_ROOTFS=/tmp/atoe2e-rootfs/guest.ext4 \
#   scripts/e2e/ready-state-acceptance-e2e.sh [runs]

set -uo pipefail

: "${ATO_FC_BIN:?set ATO_FC_BIN}"
: "${ATO_FC_KERNEL:?set ATO_FC_KERNEL}"
: "${ATO_FC_TEST_ROOTFS:?set ATO_FC_TEST_ROOTFS to the fixture guest image}"

RUNS="${1:-10}"
RUN_ID="${ATO_E2E_RUN_ID:-$$}"
PREFIX="atoe2e"
TAP="${ATO_FC_TAP:-${PREFIX}${RUN_ID: -4}}"
WORK="/tmp/${PREFIX}-${RUN_ID}/fc-work"
HOST_IP="${ATO_FC_HOST_IP:-172.31.99.1}"
GUEST_IP="${ATO_FC_GUEST_IP:-172.31.99.2}"
MASK="${ATO_FC_GUEST_MASK:-255.255.255.252}"

abort() { echo "ABORT: $*" >&2; exit 2; }

# The KVM host is typically a runner box with firecracker and /dev/kvm but NO
# cargo. Build the test binary elsewhere (`cargo test --no-run`) and point
# ATO_E2E_TEST_BIN at it; otherwise fall back to cargo when it is present.
if [ -n "${ATO_E2E_TEST_BIN:-}" ]; then
  [ -x "$ATO_E2E_TEST_BIN" ] || abort "ATO_E2E_TEST_BIN '$ATO_E2E_TEST_BIN' is not executable"
  RUNNER=("$ATO_E2E_TEST_BIN")
elif command -v cargo >/dev/null 2>&1; then
  RUNNER=(cargo test -p snapshot-builder --bin snapshot-builder --)
else
  abort "no cargo on this host: set ATO_E2E_TEST_BIN to a prebuilt test binary"
fi

# ── pre-flight: refuse to touch anything that is not ours ────────────────────
[ "${#TAP}" -le 15 ] || abort "tap name '$TAP' exceeds the 15-char IFNAMSIZ limit"
case "$TAP" in
  fctap0|ato-slot-*) abort "tap '$TAP' is a live-service name" ;;
  ${PREFIX}*) : ;;
  *) abort "tap '$TAP' does not carry the '$PREFIX' run prefix" ;;
esac
case "$WORK" in
  /tmp/ato-fc|/tmp/ato-fc/*) abort "work root '$WORK' collides with the live builder" ;;
esac
ip link show "$TAP" >/dev/null 2>&1 && abort "tap '$TAP' already exists — not deleting someone else's interface"
[ -e "$WORK" ] && abort "work root '$WORK' already exists — refusing to reuse"

# The host IP must not already be assigned ANYWHERE. Checking only the tap NAME
# is not enough and this cost a run: a previous run killed by a timeout left its
# tap behind, DOWN but still holding 172.31.99.1/24. The next run created its own
# tap with the same address, so the route to the guest was ambiguous and the
# health probe reached the dead interface — surfacing as "guest never became
# healthy within timeout", which reads like a product bug and is not one.
#
# Report the offender instead of deleting it: an address we did not assign may
# belong to something we must not touch.
EXISTING_IF="$(ip -o -4 addr show 2>/dev/null | awk -v ip="$HOST_IP" '$4 ~ "^"ip"/" {print $2}' | head -1)"
[ -n "$EXISTING_IF" ] && abort "host ip $HOST_IP is already assigned to '$EXISTING_IF' — remove it deliberately, this script will not"
EXISTING_GUEST_IF="$(ip -o -4 addr show 2>/dev/null | awk -v ip="$GUEST_IP" '$4 ~ "^"ip"/" {print $2}' | head -1)"
[ -n "$EXISTING_GUEST_IF" ] && abort "guest ip $GUEST_IP is already assigned to '$EXISTING_GUEST_IF'"

(echo >/dev/tcp/"$GUEST_IP"/8080) >/dev/null 2>&1 && abort "$GUEST_IP:8080 already answers — something is live there"

echo "=== isolation ==="
printf '  tap=%s\n  work=%s\n  host_ip=%s\n  guest_ip=%s\n  runs=%s\n' \
  "$TAP" "$WORK" "$HOST_IP" "$GUEST_IP" "$RUNS"
echo "=== live services (must stay active, untouched) ==="
systemctl is-active ato-runner-agent ato-snapshot-builder 2>&1 | paste -sd' '
LIVE_FC_BEFORE="$(pgrep -c firecracker || true)"
echo "  firecracker processes before: ${LIVE_FC_BEFORE:-0}"

mkdir -p "$WORK" || abort "cannot create $WORK"
cleanup() {
  # Run-scoped only. Never `pkill firecracker`, never `ip link del fctap0`.
  ip link show "$TAP" >/dev/null 2>&1 && ip link del "$TAP" 2>/dev/null
  rm -rf "/tmp/${PREFIX}-${RUN_ID}"
}
trap cleanup EXIT

pass=0; fail=0
for i in $(seq 1 "$RUNS"); do
  nonce="$(head -c16 /dev/urandom | od -An -tx1 | tr -d ' \n')"
  echo "=== run $i/$RUNS nonce=$nonce ==="
  start_ms=$(date +%s%3N)
  out="$(sudo -E env PATH="$PATH" \
      ATO_FC_BIN="$ATO_FC_BIN" ATO_FC_KERNEL="$ATO_FC_KERNEL" \
      ATO_FC_TEST_ROOTFS="$ATO_FC_TEST_ROOTFS" \
      ATO_FC_WORK="$WORK" ATO_FC_TAP="$TAP" \
      ATO_FC_HOST_IP="$HOST_IP" ATO_FC_GUEST_IP="$GUEST_IP" ATO_FC_GUEST_MASK="$MASK" \
      ATO_FC_ROOTFS_READONLY=1 ATO_FC_VSOCK=1 \
      ATO_E2E_NONCE="$nonce" \
      "${RUNNER[@]}" fc_kvm_production_hold_release_verify \
        --ignored --test-threads=1 --nocapture 2>&1)"
  end_ms=$(date +%s%3N)
  echo "$out" | grep -aE '^### E2E|test result:'
  if echo "$out" | grep -aq '### E2E ok=true'; then
    pass=$((pass+1)); echo "  RESULT pass total_ms=$((end_ms-start_ms))"
  else
    fail=$((fail+1)); echo "  RESULT FAIL total_ms=$((end_ms-start_ms))"
    echo "$out" | tail -30
  fi
  # Between runs: assert we left nothing of ours behind, and that the live
  # services are untouched.
  if ip link show "$TAP" >/dev/null 2>&1; then
    ip link del "$TAP" 2>/dev/null || abort "could not remove our own tap $TAP"
  fi
  rm -f "$WORK/$TAP.lock" 2>/dev/null
done

echo "=== summary ==="
echo "  pass=$pass fail=$fail of $RUNS"
LIVE_FC_AFTER="$(pgrep -c firecracker || true)"
echo "  firecracker processes after: ${LIVE_FC_AFTER:-0} (before ${LIVE_FC_BEFORE:-0})"
systemctl is-active ato-runner-agent ato-snapshot-builder 2>&1 | paste -sd' '
[ "$fail" -eq 0 ]
