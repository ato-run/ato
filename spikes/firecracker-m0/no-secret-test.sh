#!/usr/bin/env bash
# REQUIRES /dev/kvm. Seal-before-bind proof: a secret that is injected ONLY at
# runtime (post-restore) must never appear in the SEALED snapshot files. The
# snapshot from run-spike.sh is captured at /health readiness with no secret
# present; here we restore it, inject a unique sentinel into the running VM,
# stop it, and confirm the sentinel is absent from the sealed mem/vmstate (and,
# if present, from the runtime overlay/log after redaction).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"; cd "${here}"
# shellcheck disable=SC1091
. ./lib.sh
require_kvm

mem="${SPIKE_WORK}/mem.snap"; state="${SPIKE_WORK}/vmstate.snap"
[ -f "${mem}" ] || { echo "run ./run-spike.sh first (it seals BEFORE any secret)" >&2; exit 1; }

# A unique sentinel delivered ONLY post-restore (never present at seal time).
SENTINEL="SPIKE_RUNTIME_ONLY_SECRET_$$_$(cat /proc/sys/kernel/random/uuid 2>/dev/null || echo deadbeefcafef00d)"

echo "==> Restore the sealed snapshot and inject the full sentinel at runtime."
start_firecracker "${SPIKE_WORK}/api-nosecret.sock"
load_snapshot "${mem}" "${state}"
wait_healthy >/dev/null
# Inject the FULL sentinel into the running guest's memory (post-restore bind).
# The hello app stores POST /marker bodies in process memory (see build-guest.sh).
curl -fsS -X POST "http://${GUEST_IP:-172.16.0.2}:${GUEST_PORT}/marker" -d "${SENTINEL}" >/dev/null
# Confirm the running VM actually holds it (sanity: the injection worked).
got="$(curl -fsS "http://${GUEST_IP:-172.16.0.2}:${GUEST_PORT}/marker" || true)"
[ "${got}" = "${SENTINEL}" ] || { echo "FATAL: runtime injection did not take; test inconclusive" >&2; cleanup; exit 1; }
cleanup

echo "==> The SEALED files predate injection — the full sentinel must be ABSENT."
if grep -a -q "${SENTINEL}" "${mem}" "${state}"; then
  echo "FAIL: the runtime-only sentinel leaked into the sealed snapshot"; exit 1
fi
# Also assert no provider-key-shaped material was sealed.
if grep -aE -q 'sk-[A-Za-z0-9]{20,}|ghp_[A-Za-z0-9]{20,}|AKIA[0-9A-Z]{16}' "${mem}" "${state}"; then
  echo "FAIL: a provider-key-shaped string is present in the sealed snapshot"; exit 1
fi
echo "PASS: the runtime-injected secret is absent from the sealed mem/vmstate (seal-before-bind holds)"
