#!/usr/bin/env bash
# REQUIRES /dev/kvm. runner_class mismatch must be detected: a snapshot built under one
# CPU template / FC version must NOT silently restore under a different one. Validates the
# need for the fail-closed runner_class gate (capsule foundation/install_lifecycle/runner_class.rs).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"; cd "${here}"
# shellcheck disable=SC1091
. ./lib.sh
require_kvm

mem="${SPIKE_WORK}/mem.snap"; state="${SPIKE_WORK}/vmstate.snap"
[ -f "${mem}" ] || { echo "run ./run-spike.sh first" >&2; exit 1; }

echo "==> Attempt restore under a DIFFERENT cpu_template than the snapshot was built with."
echo "    Expectation: Firecracker LoadSnapshot fails, OR the guest mis-resumes/corrupts."
echo "    Either outcome validates that ato MUST gate on runner_class_id before restore."
echo "    (Set CPU_TEMPLATE in versions.env to a different family, rebuild snapshot, then run."
echo "     On the same silicon with template=none, this is expected to (dangerously) succeed —"
echo "     which is exactly why the runner_class gate is mandatory, not optional.)"
start_firecracker "${SPIKE_WORK}/api-mismatch.sock"
if load_snapshot "${mem}" "${state}" 2>"${SPIKE_WORK}/mismatch.err"; then
  echo "    LoadSnapshot SUCCEEDED under mismatched class — host-side gate is REQUIRED (no VMM safety net)."
else
  echo "    LoadSnapshot FAILED under mismatched class (VMM rejected): $(cat "${SPIKE_WORK}/mismatch.err")"
fi
cleanup
