#!/usr/bin/env bash
# U7 (#874): run the UFFD KVM smokes each in ITS OWN cargo invocation.
#
# The combined `fc_kvm_uffd*` suite flakes under `--test-threads=1` from host
# pressure — 6+ back-to-back 512 MB-mmap Firecracker boots exhaust transient host
# resources (tap teardown races, kernel memory). Each test passes reliably ALONE,
# so this runner invokes them one at a time with a clean-slate between each. This is
# the supported way to run the UFFD KVM smokes; the combined suite is NOT a release
# gate (see the classification below).
#
# ## Test classification (U7)
#   release-gate eligible (KVM-free, in default `cargo test -p snapshot`):
#     - uffd::* unit tests (evaluate/probe logic)
#     - uffd_page_server::* unit tests (page-align, region lookup, region JSON parse)
#     - uffd_mode_is_env_only_and_defaults_to_file (default File path invariant)
#     - fc_kvm_probe_uffd is #[ignore]d but deterministic; its NON-kvm assertion runs
#       via the probe-facet invariant in the default probe test
#   hardware-smoke-only (#[ignore], need /dev/kvm + userfaultfd + a bootable rootfs;
#   run via THIS script, one at a time; NOT a release gate):
#     - fc_kvm_uffd_zero_pages_plumbing        (U1a)
#     - fc_kvm_uffd_real_pages_reaches_health  (U1b)
#     - fc_kvm_uffd_cas_demand_serves_from_local_cas    (U2)
#     - fc_kvm_uffd_fault_trace_records_hotset          (U3)
#     - fc_kvm_uffd_hotset_prefetch_cuts_demand_faults  (U4)
#     - fc_kvm_uffd_corrupt_cas_chunk_fails_closed      (U5)
#     - fc_kvm_uffd_remote_readthrough_reaches_health   (U6)
#   flaky-under-pressure (do NOT release-gate): the COMBINED
#     `cargo test -p snapshot fc_kvm_uffd -- --ignored --test-threads=1` run.
#
# ## Usage
#   ATO_FC_BIN=~/bin/firecracker ATO_FC_KERNEL=~/bin/vmlinux \
#   ATO_FC_TEST_ROOTFS=~/bench/fulltest.ext4 \
#   scripts/ready-state/run-uffd-kvm-smokes.sh [test-name-substring ...]
#
# With no args, runs every fc_kvm_uffd_* smoke. Requires: sudo (tap + /dev/kvm),
# a firecracker binary, a guest kernel, and a bootable rootfs that serves /health.

set -uo pipefail

: "${ATO_FC_BIN:?set ATO_FC_BIN to the firecracker binary}"
: "${ATO_FC_KERNEL:?set ATO_FC_KERNEL to the guest kernel (vmlinux)}"
: "${ATO_FC_TEST_ROOTFS:?set ATO_FC_TEST_ROOTFS to a bootable /health rootfs}"
ATO_FC_ROOTFS_READONLY="${ATO_FC_ROOTFS_READONLY:-1}"
ATO_FC_WORK="${ATO_FC_WORK:-/tmp/ato-fc-uffd-smoke}"
ATO_FC_BOOT_TIMEOUT_S="${ATO_FC_BOOT_TIMEOUT_S:-60}"
TAP="${ATO_FC_TAP:-fctap0}"

ALL=(
  fc_kvm_uffd_zero_pages_plumbing
  fc_kvm_uffd_real_pages_reaches_health
  fc_kvm_uffd_cas_demand_serves_from_local_cas
  fc_kvm_uffd_fault_trace_records_hotset
  fc_kvm_uffd_hotset_prefetch_cuts_demand_faults
  fc_kvm_uffd_corrupt_cas_chunk_fails_closed
  fc_kvm_uffd_remote_readthrough_reaches_health
)
TESTS=("$@"); [ ${#TESTS[@]} -eq 0 ] && TESTS=("${ALL[@]}")

pass=0; fail=0
for t in "${TESTS[@]}"; do
  echo "=== $t ==="
  sudo pkill -9 firecracker 2>/dev/null || true
  sudo ip link del "$TAP" 2>/dev/null || true
  sudo rm -rf "$ATO_FC_WORK"
  sleep 2
  if sudo -E env PATH="$PATH" \
      ATO_FC_BIN="$ATO_FC_BIN" ATO_FC_KERNEL="$ATO_FC_KERNEL" ATO_FC_TEST_ROOTFS="$ATO_FC_TEST_ROOTFS" \
      ATO_FC_ROOTFS_READONLY="$ATO_FC_ROOTFS_READONLY" ATO_FC_WORK="$ATO_FC_WORK" \
      ATO_FC_BOOT_TIMEOUT_S="$ATO_FC_BOOT_TIMEOUT_S" \
      cargo test -p snapshot "$t" -- --ignored --test-threads=1 --nocapture 2>&1 \
      | grep -aE "### U[0-9].*RECEIPT|### U[0-9].*COMPARE|### U[0-9].*read-through|### U5 fail-closed|test result:"; then
    pass=$((pass+1))
  else
    fail=$((fail+1)); echo "!! $t FAILED"
  fi
done
sudo pkill -9 firecracker 2>/dev/null || true; sudo ip link del "$TAP" 2>/dev/null || true
echo "=== UFFD KVM smokes: $pass passed, $fail failed ==="
[ "$fail" -eq 0 ]
