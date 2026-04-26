#!/bin/bash
# ─────────────────────────────────────────────────────────────────
# Ato-Store Development Smoke Test
# ─────────────────────────────────────────────────────────────────
# Tests Phase 1 (Store API) and Phase 2 (CLI Auth) implementations
# against local development server (localhost:8787)
#
# Prerequisites:
# 1. Store API running: cd apps/ato-store && pnpm dev
# 2. ato CLI built: cd apps/ato-cli && cargo build --release
# 3. GitHub token set: export GITHUB_TOKEN=ghp_xxxxxxxxxxxx

set -e  # Exit on error

# Disable output buffering
export PYTHONUNBUFFERED=1
stty -icanon min 0 time 0 2>/dev/null || true

# ─────────────────────────────────────────────────────────────────
# Configuration
# ─────────────────────────────────────────────────────────────────

# Detect repository root (script is in scripts/ directory)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

STORE_URL="http://localhost:8787"
ATO_CLI="$REPO_ROOT/apps/ato-cli/target/release/ato"
TEST_TOKEN="${GITHUB_TOKEN:-}"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Test counters
TESTS_PASSED=0
TESTS_FAILED=0

# ─────────────────────────────────────────────────────────────────
# Helper Functions
# ─────────────────────────────────────────────────────────────────

log_section() {
    echo ""
    echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${BLUE}$1${NC}"
    echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
}

log_test() {
    echo -e "\n${YELLOW}▶ $1${NC}"
}

log_pass() {
    echo -e "  ${GREEN}✓ $1${NC}"
    ((++TESTS_PASSED))
}

log_fail() {
    echo -e "  ${RED}✗ $1${NC}"
    ((++TESTS_FAILED))
}

log_info() {
    echo -e "  ${BLUE}ℹ $1${NC}"
}

check_prerequisites() {
    log_section "Checking Prerequisites"

    log_info "Repository root: $REPO_ROOT"
    log_info "Ato CLI path: $ATO_CLI"

    # Check if Store API is running
    log_test "Store API reachability"

    # Use curl's built-in timeout (no need for timeout command)
    health_response=$(curl -s --connect-timeout 3 --max-time 5 "$STORE_URL/health" 2>&1)
    curl_exit=$?

    if [ $curl_exit -eq 0 ] && [ -n "$health_response" ]; then
        log_pass "Store API is running at $STORE_URL"
    else
        log_fail "Store API is not running at $STORE_URL"
        log_info "curl exit code: $curl_exit"
        log_info "Try: curl $STORE_URL/health"
        log_info "Start with: cd apps/ato-store && pnpm dev"
        exit 1
    fi

    # Check if ato CLI exists
    log_test "Ato CLI binary"
    if [ -f "$ATO_CLI" ]; then
        log_pass "Found ato CLI at $ATO_CLI"

        # Test if it's executable
        if [ -x "$ATO_CLI" ]; then
            log_pass "Binary is executable"
        else
            log_fail "Binary exists but is not executable"
            log_info "Fix with: chmod +x $ATO_CLI"
            exit 1
        fi
    else
        log_fail "Ato CLI not found at $ATO_CLI"
        log_info "Build with: cd $REPO_ROOT/apps/ato-cli && cargo build --release"
        exit 1
    fi
    
    # Check if GitHub token is set (optional for API tests)
    log_test "GitHub token"
    if [ -n "$TEST_TOKEN" ]; then
        log_pass "GitHub token is set (${#TEST_TOKEN} characters)"
    else
        log_info "GitHub token not set (CLI auth tests will be skipped)"
        log_info "Set with: export GITHUB_TOKEN=ghp_xxxxxxxxxxxx"
    fi

    # Check for required commands
    log_test "Required commands"
    MISSING_CMDS=""
    command -v curl >/dev/null 2>&1 || MISSING_CMDS="$MISSING_CMDS curl"
    command -v jq >/dev/null 2>&1 || MISSING_CMDS="$MISSING_CMDS jq"

    if [ -z "$MISSING_CMDS" ]; then
        log_pass "Required commands found (curl, jq)"
    else
        log_fail "Missing commands:$MISSING_CMDS"
        log_info "Install with: brew install$MISSING_CMDS  # macOS"
        exit 1
    fi
}

# ─────────────────────────────────────────────────────────────────
# Store API Tests (Phase 1)
# ─────────────────────────────────────────────────────────────────

test_store_api() {
    log_section "Phase 1: Store API Tests"
    
    # Test 1: Health endpoint
    log_test "GET /health"
    response=$(curl -s "$STORE_URL/health")
    if [ -n "$response" ]; then
        log_pass "Health endpoint responds"
        log_info "Response: $response"
    else
        log_fail "Health endpoint did not respond"
    fi
    
    # Test 2: Well-known registry discovery
    log_test "GET /.well-known/capsule.json"
    response=$(curl -s "$STORE_URL/.well-known/capsule.json")
    
    if echo "$response" | jq . > /dev/null 2>&1; then
        log_pass "Well-known endpoint returns valid JSON"
        
        # Check for required fields
        url=$(echo "$response" | jq -r '.url')
        name=$(echo "$response" | jq -r '.name')
        public_key=$(echo "$response" | jq -r '.public_key')
        version=$(echo "$response" | jq -r '.version')
        
        if [ "$url" = "$STORE_URL" ]; then
            log_pass "Registry URL matches: $url"
        else
            log_fail "Registry URL mismatch: expected $STORE_URL, got $url"
        fi
        
        if [ -n "$name" ] && [ "$name" != "null" ]; then
            log_pass "Registry name: $name"
        else
            log_fail "Registry name missing"
        fi
        
        if [ -n "$public_key" ] && [ "$public_key" != "null" ]; then
            log_pass "Public key (DID): $public_key"
            
            # Verify it's a valid did:key format
            if [[ "$public_key" =~ ^did:key:z6Mk ]]; then
                log_pass "Public key has valid did:key format"
            else
                log_fail "Public key format invalid: $public_key"
            fi
        else
            log_fail "Public key missing (Phase 1 requirement)"
        fi
        
        if [ "$version" = "1" ]; then
            log_pass "API version: $version"
        else
            log_fail "API version unexpected: $version"
        fi
    else
        log_fail "Well-known endpoint did not return valid JSON"
        log_info "Response: $response"
    fi
    
    # Test 3: List capsules endpoint
    log_test "GET /v1/capsules"
    response=$(curl -s "$STORE_URL/v1/capsules")
    
    if echo "$response" | jq . > /dev/null 2>&1; then
        log_pass "Capsules list endpoint returns valid JSON"
        
        capsule_count=$(echo "$response" | jq '.capsules | length')
        log_info "Found $capsule_count capsules"
        
        # Check response structure
        if echo "$response" | jq -e '.capsules' > /dev/null 2>&1; then
            log_pass "Response has 'capsules' array"
        else
            log_fail "Response missing 'capsules' array"
        fi
        
        if echo "$response" | jq -e '.next_cursor' > /dev/null 2>&1; then
            log_pass "Response has 'next_cursor' field"
        fi
    else
        log_fail "Capsules list endpoint did not return valid JSON"
        log_info "Response: $response"
    fi
    
    # Test 4: Search capsules
    log_test "GET /v1/capsules?q=test"
    response=$(curl -s "$STORE_URL/v1/capsules?q=test")
    
    if echo "$response" | jq . > /dev/null 2>&1; then
        log_pass "Search endpoint returns valid JSON"
    else
        log_fail "Search endpoint did not return valid JSON"
    fi
    
    # Test 5: Test non-existent capsule (should 404)
    log_test "GET /v1/capsules/nonexistent (expect 404)"
    status_code=$(curl -s -o /dev/null -w "%{http_code}" "$STORE_URL/v1/capsules/nonexistent")
    
    if [ "$status_code" = "404" ]; then
        log_pass "Non-existent capsule returns 404"
    else
        log_fail "Non-existent capsule returned $status_code (expected 404)"
    fi
}

# ─────────────────────────────────────────────────────────────────
# CLI Auth Tests (Phase 2)
# ─────────────────────────────────────────────────────────────────

test_cli_auth() {
    log_section "Phase 2: CLI Auth Tests"
    
    # Clean up any existing credentials
    rm -f ~/.capsule/credentials.json
    
    # Test 1: Auth status when not logged in
    log_test "ato auth (not authenticated)"
    output=$("$ATO_CLI" auth 2>&1) || {
        log_fail "Command failed"
        return
    }

    if echo "$output" | grep -q "Not authenticated"; then
        log_pass "Auth status correctly shows 'Not authenticated'"
    else
        log_fail "Auth status did not show 'Not authenticated'"
        log_info "Output: $output"
    fi
    
    # Skip GitHub token tests if not provided
    if [ -z "$TEST_TOKEN" ]; then
        log_info "Skipping login tests (no GitHub token provided)"
        return
    fi
    
    # Test 2: Login with GitHub token
    log_test "ato login --token <github-token>"
    output=$("$ATO_CLI" login --token "$TEST_TOKEN" 2>&1) || {
        log_fail "Login command failed"
        return
    }
    
    if echo "$output" | grep -q "Authenticated as"; then
        log_pass "Login successful"
        username=$(echo "$output" | grep "Authenticated as" | sed 's/.*@//')
        log_info "Logged in as: @$username"
    else
        log_fail "Login failed"
        log_info "Output: $output"
        return
    fi
    
    # Test 3: Verify credentials file exists
    log_test "Credentials file created"
    if [ -f ~/.capsule/credentials.json ]; then
        log_pass "Credentials file exists at ~/.capsule/credentials.json"
        
        # Verify JSON structure
        if jq . ~/.capsule/credentials.json > /dev/null 2>&1; then
            log_pass "Credentials file is valid JSON"
            
            github_token=$(jq -r '.github_token' ~/.capsule/credentials.json)
            github_username=$(jq -r '.github_username' ~/.capsule/credentials.json)
            
            if [ -n "$github_token" ] && [ "$github_token" != "null" ]; then
                log_pass "GitHub token stored"
            else
                log_fail "GitHub token missing in credentials"
            fi
            
            if [ -n "$github_username" ] && [ "$github_username" != "null" ]; then
                log_pass "GitHub username stored: @$github_username"
            else
                log_fail "GitHub username missing in credentials"
            fi
        else
            log_fail "Credentials file is not valid JSON"
        fi
    else
        log_fail "Credentials file not created"
    fi
    
    # Test 4: Auth status when logged in
    log_test "ato auth (authenticated)"
    output=$("$ATO_CLI" auth 2>&1)
    
    if echo "$output" | grep -q "Authenticated"; then
        log_pass "Auth status shows 'Authenticated'"
    else
        log_fail "Auth status did not show 'Authenticated'"
        log_info "Output: $output"
    fi
    
    # Test 5: Logout
    log_test "ato logout"
    output=$("$ATO_CLI" logout 2>&1)
    
    if echo "$output" | grep -q "Logged out successfully"; then
        log_pass "Logout successful"
    else
        log_fail "Logout failed"
        log_info "Output: $output"
    fi
    
    # Test 6: Verify credentials file deleted
    log_test "Credentials file deleted after logout"
    if [ ! -f ~/.capsule/credentials.json ]; then
        log_pass "Credentials file deleted"
    else
        log_fail "Credentials file still exists"
    fi
    
    # Test 7: Auth status after logout
    log_test "ato auth (after logout)"
    output=$("$ATO_CLI" auth 2>&1)
    
    if echo "$output" | grep -q "Not authenticated"; then
        log_pass "Auth status correctly shows 'Not authenticated' after logout"
    else
        log_fail "Auth status incorrect after logout"
        log_info "Output: $output"
    fi
}

# ─────────────────────────────────────────────────────────────────
# Registry Discovery Tests
# ─────────────────────────────────────────────────────────────────

test_registry_discovery() {
    log_section "Registry Discovery Tests"
    
    # Test CLI registry resolver
    log_test "ato registry resolve localhost"
    output=$("$ATO_CLI" registry resolve localhost 2>&1)
    
    if echo "$output" | grep -q "Registry for localhost"; then
        log_pass "Registry resolver works for localhost"
        log_info "$(echo "$output" | grep URL)"
    else
        log_fail "Registry resolver failed for localhost"
        log_info "Output: $output"
    fi
}

# ─────────────────────────────────────────────────────────────────
# Integration Tests
# ─────────────────────────────────────────────────────────────────

test_integration() {
    log_section "Integration Tests"
    
    log_test "End-to-end registry discovery and list"
    
    # Resolve registry
    registry_info=$("$ATO_CLI" registry resolve localhost --json 2>&1)
    
    if echo "$registry_info" | jq . > /dev/null 2>&1; then
        log_pass "Registry resolved via CLI"
        
        registry_url=$(echo "$registry_info" | jq -r '.url')
        log_info "Registry URL: $registry_url"
        
        # Fetch capsules from resolved registry
        capsules_response=$(curl -s "$registry_url/v1/capsules")
        
        if echo "$capsules_response" | jq . > /dev/null 2>&1; then
            log_pass "Capsules list fetched from resolved registry"
            count=$(echo "$capsules_response" | jq '.capsules | length')
            log_info "Capsules available: $count"
        else
            log_fail "Failed to fetch capsules from resolved registry"
        fi
    else
        log_fail "Registry resolution failed"
        log_info "Output: $registry_info"
    fi
}

# ─────────────────────────────────────────────────────────────────
# Main Execution
# ─────────────────────────────────────────────────────────────────

main() {
    echo ""
    echo "╔═══════════════════════════════════════════════════════════════╗"
    echo "║     Ato-Store Development Smoke Test Suite                   ║"
    echo "║     Testing Phases 1 & 2                                     ║"
    echo "╚═══════════════════════════════════════════════════════════════╝"
    
    check_prerequisites
    test_store_api
    test_cli_auth
    test_registry_discovery
    test_integration
    
    # Summary
    log_section "Test Summary"
    
    TOTAL_TESTS=$((TESTS_PASSED + TESTS_FAILED))
    
    echo ""
    echo "  Total tests:  $TOTAL_TESTS"
    echo -e "  ${GREEN}Passed:       $TESTS_PASSED${NC}"
    
    if [ $TESTS_FAILED -eq 0 ]; then
        echo -e "  ${GREEN}Failed:       $TESTS_FAILED${NC}"
        echo ""
        echo -e "${GREEN}╔═══════════════════════════════════════════════════════════════╗${NC}"
        echo -e "${GREEN}║  ✓ ALL TESTS PASSED                                          ║${NC}"
        echo -e "${GREEN}╚═══════════════════════════════════════════════════════════════╝${NC}"
        exit 0
    else
        echo -e "  ${RED}Failed:       $TESTS_FAILED${NC}"
        echo ""
        echo -e "${RED}╔═══════════════════════════════════════════════════════════════╗${NC}"
        echo -e "${RED}║  ✗ SOME TESTS FAILED                                         ║${NC}"
        echo -e "${RED}╚═══════════════════════════════════════════════════════════════╝${NC}"
        exit 1
    fi
}

# Check for required commands
command -v curl >/dev/null 2>&1 || { echo "Error: curl is required but not installed."; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "Error: jq is required but not installed."; exit 1; }

main "$@"
