#!/bin/bash
# =============================================================================
# Pre-commit hook for nacelle
# Runs quick checks before each commit (format + clippy)
# 
# Installation:
#   chmod +x scripts/pre-commit-hook.sh
#   ln -sf ../../scripts/pre-commit-hook.sh .git/hooks/pre-commit
# =============================================================================

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Determine project root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ "$SCRIPT_DIR" == *".git/hooks"* ]]; then
    PROJECT_ROOT="$(git rev-parse --show-toplevel)"
else
    PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
fi

cd "$PROJECT_ROOT"

echo -e "${YELLOW}⚡ Pre-commit checks (nacelle)...${NC}"

# Step 1: Format check (fast)
echo -n "  Formatting... "
if cargo fmt --all -- --check 2>/dev/null; then
    echo -e "${GREEN}✓${NC}"
else
    echo -e "${RED}✗${NC}"
    echo -e "${RED}Run 'cargo fmt --all' to fix formatting${NC}"
    exit 1
fi

# Step 2: Clippy (lib/bins only for speed)
echo -n "  Clippy... "
if cargo clippy --lib --bins -- -D warnings 2>/dev/null; then
    echo -e "${GREEN}✓${NC}"
else
    echo -e "${RED}✗${NC}"
    echo -e "${RED}Fix clippy warnings before committing${NC}"
    exit 1
fi

echo -e "${GREEN}✓ Pre-commit checks passed${NC}"
