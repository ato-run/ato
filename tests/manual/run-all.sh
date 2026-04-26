#!/bin/bash
# =============================================================================
# run-all.sh — Run all 15 pre-release manual test suites
# Usage: ./run-all.sh [--from N] [--only N]
#   --from N   start from suite N (1-15)
#   --only N   run only suite N
# =============================================================================
set -uo pipefail
export ATO_TEST_AUTO=1
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/config.sh"

SUITES=(
    "01-install-upgrade"
    "02-gpu-accelerator"
    "03-first-run-download"
    "04-sandbox-boundary"
    "05-cross-os"
    "06-share-url"
    "07-ato-desktop-ux"
    "08-trust-ux"
    "09-network-isolation"
    "10-error-messages"
    "11-ato-api"
    "12-toolchain-interference"
    "13-longtail-envs"
    "14-doc-alignment"
    "15-dogfooding"
)

FROM=1
ONLY=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --from) FROM="$2"; shift 2 ;;
        --only) ONLY="$2"; shift 2 ;;
        *) echo "Unknown arg: $1"; exit 1 ;;
    esac
done

echo "╔══════════════════════════════════════════════════════╗"
echo "║     ato Pre-Release Manual Test Suite — run-all     ║"
echo "╚══════════════════════════════════════════════════════╝"
echo "Results dir: $RESULTS_DIR"
echo ""

TOTAL_PASS=0
TOTAL_FAIL=0
TOTAL_SKIP=0
FAILED_SUITES=()

for i in "${!SUITES[@]}"; do
    suite_n=$((i + 1))
    suite="${SUITES[$i]}"
    test_script="$SCRIPT_DIR/$suite/test.sh"

    [ $ONLY -ne 0 ] && [ $suite_n -ne $ONLY ] && continue
    [ $suite_n -lt $FROM ] && continue

    if [ ! -f "$test_script" ]; then
        echo "  ⚠  $suite/test.sh not found — skipping"
        continue
    fi

    echo ""
    echo "▶ Running suite $suite_n/15: $suite"
    echo "────────────────────────────────────"

    bash "$test_script" || true
    exit_code=$?

    # Read per-suite result file to aggregate totals
    result_file=$(ls "$RESULTS_DIR/result_$(printf '%02d' $suite_n)"_*.log 2>/dev/null | head -1 || true)
    if [ -n "$result_file" ] && [ -f "$result_file" ]; then
        p=$(grep -c "^\[PASS\]" "$result_file" 2>/dev/null || true)
        f=$(grep -c "^\[FAIL\]" "$result_file" 2>/dev/null || true)
        s=$(grep -c "^\[SKIP\]" "$result_file" 2>/dev/null || true)
        TOTAL_PASS=$((TOTAL_PASS + ${p:-0}))
        TOTAL_FAIL=$((TOTAL_FAIL + ${f:-0}))
        TOTAL_SKIP=$((TOTAL_SKIP + ${s:-0}))
        [ "$f" -gt 0 ] && FAILED_SUITES+=("$suite")
    fi
done

TOTAL=$((TOTAL_PASS + TOTAL_FAIL + TOTAL_SKIP))

echo ""
echo "╔══════════════════════════════════════════════════════╗"
echo "║               COMBINED RESULTS SUMMARY              ║"
echo "╚══════════════════════════════════════════════════════╝"
printf "  Total:  %d  |  ✓ Pass: %d  |  ✗ Fail: %d  |  ○ Skip: %d\n" \
    "$TOTAL" "$TOTAL_PASS" "$TOTAL_FAIL" "$TOTAL_SKIP"

if [ "${#FAILED_SUITES[@]}" -gt 0 ]; then
    echo ""
    echo "  Failed suites:"
    for s in "${FAILED_SUITES[@]}"; do
        echo "    ✗ $s"
    done
fi

echo ""
echo "  Results saved to: $RESULTS_DIR/"
echo ""

[ "$TOTAL_FAIL" -eq 0 ] && exit 0 || exit 1
