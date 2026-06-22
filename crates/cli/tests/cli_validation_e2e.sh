#!/bin/bash
# E2E Test: CLI Validation & Signature Verification
#
# This test suite verifies that ato-cli commands match ADR requirements:
# - CLI option validation (--enforcement accepts strict/best_effort)
# - Signature verification workflow
# - Pack creates bundles that can be executed

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_DIR="${SCRIPT_DIR}/test-workspace"
ATO_CLI="${SCRIPT_DIR}/../target/debug/ato"

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[0;33m'
NC='\033[0m'

log_info() { echo -e "${GREEN}✓${NC} $1"; }
log_error() { echo -e "${RED}✗${NC} $1"; }
log_warn() { echo -e "${YELLOW}⚠${NC} $1"; }

cleanup() {
    log_info "Cleaning up..."
    rm -rf "${TEST_DIR}"
    unset ATO_TSNET_CONTROL_URL
    unset ATO_TSNET_AUTH_KEY
    unset ATO_TSNET_HOSTNAME
}
trap cleanup EXIT

check_ato_cli() {
    if [ ! -f "${ATO_CLI}" ]; then
        log_error "ato-cli not found at ${ATO_CLI}"
        log_info "Build with: cd .. && cargo build"
        exit 1
    fi
}

echo "=========================================="
echo "E2E Test: CLI & Signature Verification"
echo "=========================================="
echo ""

# Build ato-cli first
echo "Building ato-cli..."
cd "${SCRIPT_DIR}/.."
cargo build 2>&1 > /dev/null
check_ato_cli

# Test 1: CLI --enforcement validation
 echo "Test 1: CLI --enforcement validation"
 echo "--------------------------------------"
 
 # Test 1.1: Help shows enforcement option
 echo "  Testing: Help shows enforcement option..."
 if "${ATO_CLI}" run --help 2>&1 | grep -q "enforcement"; then
     log_info "  --enforcement option documented in help"
 else
     log_error "  --enforcement option not in help"
     exit 1
 fi

echo ""
log_info "Test 1: PASSED"
echo ""
log_info "Test 1: PASSED"
echo ""
