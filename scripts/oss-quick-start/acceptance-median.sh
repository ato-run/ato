#!/usr/bin/env bash
# Run acceptance.sh several times and report the median phase times, so a
# single loaded run cannot be mistaken for the normal startup cost.
#
# Usage:
#   acceptance-median.sh --runs 3 --ato <ato> --capsules <dir> [--install-cmd <cmd>]
set -euo pipefail

PYTHON=python
if python3 -c "import sys" >/dev/null 2>&1; then PYTHON=python3; fi

runs=3
base_port=18840
args=()

while [ $# -gt 0 ]; do
  case "$1" in
    --runs) runs=$2; shift 2 ;;
    --port) base_port=$2; shift 2 ;;
    --) shift; args+=("$@"); break ;;
    *) args+=("$1"); shift ;;
  esac
done

opens=()
resumes=()
for run in $(seq 1 "$runs"); do
  port=$((base_port + run))
  output=$(bash "$(dirname "$0")/acceptance.sh" --port "$port" "${args[@]}") || {
    echo "$output" >&2
    exit 1
  }
  open_ms=$(sed -n 's/^  open:    \([0-9]*\)ms$/\1/p' <<<"$output")
  resume_ms=$(sed -n 's/^  resume:  \([0-9]*\)ms$/\1/p' <<<"$output")
  echo "run $run: open ${open_ms}ms resume ${resume_ms}ms"
  opens+=("$open_ms")
  resumes+=("$resume_ms")
  sleep 1
done

median() {
  "$PYTHON" - "$@" <<'PY'
import statistics
import sys
values = sorted(int(value) for value in sys.argv[1:])
print(round(statistics.median(values)))
PY
}

open_median=$(median "${opens[@]}")
resume_median=$(median "${resumes[@]}")
echo
echo "PASS"
echo "  open median:   ${open_median}ms (${opens[*]})"
echo "  resume median: ${resume_median}ms (${resumes[*]})"
