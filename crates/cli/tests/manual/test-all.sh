#!/bin/bash
# =============================================================================
# Run all manual test suites for ato-cli
# =============================================================================
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/config.sh"

echo "=========================================="
echo " ato-cli Manual Test Suites"
echo " $(date '+%Y-%m-%d %H:%M:%S')"
echo "=========================================="

check_ato

SUITES=(
    "01-group1-env/test-group1-env.sh"
    "02-group2-pm/test-group2-pm.sh"
    "03-group3-secrets/test-group3-secrets.sh"
    "04-group4-targets/test-group4-targets.sh"
    "05-group5-edge/test-group5-edge.sh"
)

PASSED=0; FAILED=0; FAILED_SUITES=()
for suite in "${SUITES[@]}"; do
    echo ""
    echo "=========================================="
    echo "Running: $suite"
    echo "=========================================="
    if bash "$SCRIPT_DIR/$suite" < /dev/null; then
        PASSED=$((PASSED + 1))
        print_status "PASS" "$suite"
    else
        FAILED=$((FAILED + 1))
        FAILED_SUITES+=("$suite")
        print_status "FAIL" "$suite"
    fi
done

echo ""
echo "=========================================="
echo "TOTAL: $PASSED passed, $FAILED failed"
if [ ${#FAILED_SUITES[@]} -gt 0 ]; then
    echo "FAILED SUITES:"
    for s in "${FAILED_SUITES[@]}"; do echo "  - $s"; done
fi
echo "=========================================="
[ $FAILED -eq 0 ] && exit 0 || exit 1
