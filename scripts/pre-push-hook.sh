#!/bin/bash
# =============================================================================
# Pre-push hook for capsuled
# Runs all checks before pushing to main branch
# 
# Installation:
#   chmod +x scripts/pre-push-hook.sh
#   ln -sf ../../scripts/pre-push-hook.sh .git/hooks/pre-push
# =============================================================================

set -e

# Ignore SIGPIPE to prevent exit code 141
trap '' PIPE

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Determine project root - handle both direct execution and git hook execution
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ "$SCRIPT_DIR" == *".git/hooks"* ]]; then
    # Running as git hook - find project root from git
    PROJECT_ROOT="$(git rev-parse --show-toplevel)"
else
    # Running directly from scripts/
    PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
fi

log_step() {
    echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${BLUE}  $1${NC}"
    echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
}

log_success() {
    echo -e "${GREEN}✓${NC} $1"
}

log_fail() {
    echo -e "${RED}✗${NC} $1"
}

# Check if pushing to main
check_branch() {
    local remote="$1"
    local url="$2"
    
    while read local_ref local_sha remote_ref remote_sha; do
        if [[ "$remote_ref" == "refs/heads/main" ]]; then
            echo "Pushing to main branch - running full pre-push checks..."
            return 0
        fi
    done
    
    echo "Not pushing to main - skipping pre-push checks"
    exit 0
}

# Only run checks when pushing to main
if [ -t 0 ]; then
    # Interactive mode - check current branch
    current_branch=$(git rev-parse --abbrev-ref HEAD)
    if [ "$current_branch" != "main" ]; then
        echo -e "${YELLOW}Not on main branch - running quick checks only${NC}"
        QUICK_MODE=1
    fi
fi

cd "$PROJECT_ROOT"

echo ""
echo -e "${YELLOW}╔═══════════════════════════════════════════════════════════════╗${NC}"
echo -e "${YELLOW}║           CAPSULED PRE-PUSH VERIFICATION                      ║${NC}"
echo -e "${YELLOW}╚═══════════════════════════════════════════════════════════════╝${NC}"
echo ""

FAILED=0

# Step 1: Format check
log_step "1/5: Checking code formatting"
if cargo fmt --all -- --check; then
    log_success "Code formatting OK"
else
    log_fail "Code formatting issues found"
    echo "  Run: cargo fmt --all"
    FAILED=1
fi
echo ""

# Step 2: Clippy
log_step "2/5: Running Clippy lints"
# Note: Using --lib --bins to avoid test file deprecation warnings
if cargo clippy --lib --bins -- -D warnings 2>/dev/null; then
    log_success "Clippy checks passed"
else
    log_fail "Clippy warnings found"
    FAILED=1
fi
echo ""

# Step 3: Unit tests
log_step "3/5: Running unit tests"
if cargo test --workspace --lib 2>/dev/null; then
    log_success "Unit tests passed"
else
    log_fail "Unit tests failed"
    FAILED=1
fi
echo ""

# Step 4: Build release
log_step "4/5: Building release binaries"
if cargo build --release --bin capsuled -p capsuled && cargo build --release --bin capsule -p capsule-cli 2>/dev/null; then
    log_success "Release build succeeded"
else
    log_fail "Release build failed"
    FAILED=1
fi
echo ""

# Step 5: E2E tests - skipped in pre-push (run on CI instead)
# E2E tests require 2+ minutes and can have timing issues when run from git hooks
# They are always run in CI, ensuring coverage without blocking developer workflow
log_step "5/5: E2E tests (deferred to CI)"
echo -e "${YELLOW}  ⏭  E2E tests skipped in pre-push (will run on CI)${NC}"
echo -e "     Run manually: ./scripts/e2e-test.sh --quick"
echo ""

# Summary
echo -e "${YELLOW}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
if [ $FAILED -eq 0 ]; then
    echo -e "${GREEN}  ✅ All pre-push checks passed!${NC}"
    echo -e "${YELLOW}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo ""
    exit 0
else
    echo -e "${RED}  ❌ Pre-push checks failed!${NC}"
    echo -e "${YELLOW}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo ""
    echo -e "${RED}Push blocked. Please fix the issues above before pushing.${NC}"
    exit 1
fi
