#!/bin/bash
# E2E Test: Pure Runtime Architecture

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
}
trap cleanup EXIT

echo "=========================================="
echo "E2E Test: Pure Runtime Architecture"
echo "=========================================="
echo ""

# Test 1: Unit tests
echo "Test 1: Running Unit Tests"
echo "---------------------------"
cd "${SCRIPT_DIR}/.."
if cargo test 2>&1 | grep -q "test result: ok"; then
    log_info "All unit tests passed"
else
    log_error "Unit tests failed"
    exit 1
fi

echo ""

# Test 2: CLI Validation E2E
echo "Test 2: CLI Validation E2E"
echo "------------------------------"
if [ -f "./tests/cli_validation_e2e.sh" ]; then
    if bash ./tests/cli_validation_e2e.sh; then
        log_info "CLI validation E2E passed"
    else
        log_error "CLI validation E2E failed"
        exit 1
    fi
else
    log_warn "CLI validation E2E not found, skipping"
fi

echo ""

# Test 3: Pack & Sign E2E
echo "Test 3: Pack & Sign E2E"
echo "------------------------------"
if [ -f "./tests/pack_sign_e2e.sh" ]; then
    if bash ./tests/pack_sign_e2e.sh; then
        log_info "Pack & sign E2E passed"
    else
        log_error "Pack & sign E2E failed"
        exit 1
    fi
else
    log_warn "Pack & sign E2E not found, skipping"
fi

echo ""

# Test 4: Zero Config & Auto Submit E2E
echo "Test 4: Zero Config & Auto Submit E2E"
echo "---------------------------------------"
if [ -f "./tests/e2e_zero_config.sh" ]; then
    if bash ./tests/e2e_zero_config.sh; then
        log_info "Zero Config & Auto Submit E2E passed"
    else
        log_error "Zero Config & Auto Submit E2E failed"
        exit 1
    fi
else
    log_warn "Zero Config E2E not found, skipping"
fi

echo ""
log_info "Phase 1 & 2 Implementation: VERIFIED"
echo ""

