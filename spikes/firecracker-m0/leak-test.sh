#!/usr/bin/env bash
# REQUIRES /dev/kvm. Cross-restore state-leak check — the most important M0 result and the
# future permanent regression test for `ato run`. A snapshot is a clone: every restore must
# start from identical sealed state and must NOT see a marker written into a PRIOR restore.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"; cd "${here}"
# shellcheck disable=SC1091
. ./lib.sh
require_kvm

N="${1:-20}"
mem="${SPIKE_WORK}/mem.snap"; state="${SPIKE_WORK}/vmstate.snap"
[ -f "${mem}" ] || { echo "run ./run-spike.sh first to produce the snapshot" >&2; exit 1; }

leaks=0
for i in $(seq 1 "${N}"); do
  start_firecracker "${SPIKE_WORK}/api-leak-${i}.sock"
  load_snapshot "${mem}" "${state}"; wait_healthy >/dev/null
  # A fresh restore must report an EMPTY marker (sealed state had none, sanitizer reset any).
  got="$(curl -fsS "http://${GUEST_IP:-172.16.0.2}:${GUEST_PORT}/marker" || true)"
  if [ -n "${got}" ]; then
    echo "    restore #${i}: LEAK — saw prior marker '${got}'"; leaks=$((leaks+1))
  fi
  # Now write a unique marker into THIS restore's memory; it must not survive to the next.
  curl -fsS -X POST "http://${GUEST_IP:-172.16.0.2}:${GUEST_PORT}/marker" -d "marker-${i}-$$" >/dev/null
  cleanup
done

echo "==> ${N} cycles, leaks=${leaks}"
[ "${leaks}" -eq 0 ] && echo "PASS: zero cross-restore state leak" || { echo "FAIL: state leaks detected"; exit 1; }
