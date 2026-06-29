#!/usr/bin/env bash
# REQUIRES /dev/kvm. Boot -> healthcheck -> snapshot -> restore -> re-expose -> measure.
# Writes ${SPIKE_WORK}/receipt.json. Repeat N times to measure restore latency distribution.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"; cd "${here}"
# shellcheck disable=SC1091
. ./lib.sh
require_kvm

N="${1:-20}"
mem="${SPIKE_WORK}/mem.snap"
state="${SPIKE_WORK}/vmstate.snap"

echo "==> Boot a fresh VM and seal a snapshot (the build-phase capture)"
start_firecracker "${SPIKE_WORK}/api-build.sock"
configure_vm
boot_vm
boot_ms="$(wait_healthy)"
echo "    cold boot -> healthy: ${boot_ms} ms"
create_snapshot "${mem}" "${state}"
echo "    snapshot written: $(du -h "${mem}" "${state}" | tr '\n' ' ')"
cleanup

echo "==> Restore the snapshot ${N} times (File mem backend) and measure restore->ready"
restore_ms_list=()
for i in $(seq 1 "${N}"); do
  start_firecracker "${SPIKE_WORK}/api-restore-${i}.sock"
  # host-side: (re)create TAP fc-tap0 + NAT before load; guest re-ups iface (guest-agent).
  t0=$(date +%s%3N)
  load_snapshot "${mem}" "${state}"
  ms="$(wait_healthy)"; t1=$(date +%s%3N)
  echo "    restore #${i}: ready in ${ms} ms (load+ready ${RANDOM:+wall=$((t1-t0))ms})"
  restore_ms_list+=("${ms}")
  cleanup
done

# crude p50/p95
sorted=$(printf '%s\n' "${restore_ms_list[@]}" | sort -n)
p50=$(echo "${sorted}" | awk '{a[NR]=$1} END{print a[int(NR*0.5)+ (NR*0.5==int(NR*0.5)?0:1)]}')
p95=$(echo "${sorted}" | awk '{a[NR]=$1} END{print a[int(NR*0.95)+ (NR*0.95==int(NR*0.95)?0:1)]}')

cat > "${SPIKE_WORK}/receipt.json" <<JSON
{
  "fc_version": "${FC_VERSION}",
  "arch": "${FC_ARCH}",
  "cpu_template": "${CPU_TEMPLATE}",
  "cold_boot_ms": ${boot_ms},
  "restore_samples": ${N},
  "restore_ms_p50": ${p50:-null},
  "restore_ms_p95": ${p95:-null},
  "mem_snap_bytes": $(stat -c%s "${mem}"),
  "vmstate_snap_bytes": $(stat -c%s "${state}")
}
JSON
echo "==> receipt: ${SPIKE_WORK}/receipt.json"
cat "${SPIKE_WORK}/receipt.json"
