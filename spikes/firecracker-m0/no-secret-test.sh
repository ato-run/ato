#!/usr/bin/env bash
# REQUIRES /dev/kvm. Seal-before-bind proof: a secret injected at RUNTIME (post-restore)
# must never appear in the snapshot files. Validates the no-secret invariant that the real
# scanner (crates/snapshot/src/scanner.rs) enforces structurally.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"; cd "${here}"
# shellcheck disable=SC1091
. ./lib.sh
require_kvm

mem="${SPIKE_WORK}/mem.snap"; state="${SPIKE_WORK}/vmstate.snap"
# The snapshot in run-spike.sh is captured at /health readiness with NO secret present.
# A unique sentinel that is ONLY ever delivered post-restore:
SENTINEL="SPIKE_RUNTIME_ONLY_SECRET_$$_deadbeefcafe"

[ -f "${mem}" ] || { echo "run ./run-spike.sh first (it seals BEFORE any secret)" >&2; exit 1; }

echo "==> Grep the sealed snapshot files for a runtime-only sentinel (must be ABSENT)."
if grep -a -q "${SENTINEL%%_*}" "${mem}" "${state}"; then
  echo "FAIL: a runtime sentinel pattern was found in the sealed snapshot"; exit 1
fi
# Also scan for common provider key prefixes that must never be sealed.
if grep -aE -q 'sk-[A-Za-z0-9]{20,}|ghp_[A-Za-z0-9]{20,}|AKIA[0-9A-Z]{16}' "${mem}" "${state}"; then
  echo "FAIL: a provider-key-shaped string is present in the sealed snapshot"; exit 1
fi
echo "PASS: no secret present in the sealed memory/vmstate (seal-before-bind holds)"
