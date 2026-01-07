#!/bin/bash
# =============================================================================
# Push helper script for capsuled
# Asks for confirmation before pushing
# =============================================================================

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

cd "$PROJECT_ROOT"

echo ""
echo -e "${YELLOW}╔═══════════════════════════════════════════════════════════════╗${NC}"
echo -e "${YELLOW}║              CAPSULED PUSH CONFIRMATION                        ║${NC}"
echo -e "${YELLOW}╚═══════════════════════════════════════════════════════════════╝${NC}"
echo ""

# Show current status
echo -e "${BLUE}Current branch:${NC} $(git rev-parse --abbrev-ref HEAD)"
echo -e "${BLUE}Commits to push:${NC}"
git log --oneline origin/main..HEAD 2>/dev/null || echo "  (new branch or no upstream)"
echo ""

# Ask if pre-push checks were run
echo -e "${YELLOW}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo -e "${YELLOW}Pre-push checklist:${NC}"
echo "  1. cargo fmt --all -- --check"
echo "  2. cargo clippy --lib --bins -- -D warnings"
echo "  3. cargo test --workspace --lib"
echo "  4. cargo build --release"
echo ""
echo -e "Run checks now? ${BLUE}(y)${NC} Run checks / ${BLUE}(s)${NC} Skip and push / ${BLUE}(n)${NC} Cancel"
echo -e "${YELLOW}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"

read -r -p "Choice [y/s/n]: " choice

case "$choice" in
    y|Y)
        echo ""
        echo -e "${BLUE}Running pre-push checks...${NC}"
        echo ""
        
        # Format check
        echo -n "  Formatting... "
        if cargo fmt --all -- --check 2>/dev/null; then
            echo -e "${GREEN}✓${NC}"
        else
            echo -e "${RED}✗${NC}"
            echo -e "${RED}Run 'cargo fmt --all' to fix${NC}"
            exit 1
        fi
        
        # Clippy
        echo -n "  Clippy... "
        if cargo clippy --lib --bins -- -D warnings 2>/dev/null; then
            echo -e "${GREEN}✓${NC}"
        else
            echo -e "${RED}✗${NC}"
            exit 1
        fi
        
        # Unit tests
        echo -n "  Unit tests... "
        if cargo test --workspace --lib 2>/dev/null; then
            echo -e "${GREEN}✓${NC}"
        else
            echo -e "${RED}✗${NC}"
            exit 1
        fi
        
        # Build
        echo -n "  Build... "
        if cargo build --release --bin capsuled -p capsuled 2>/dev/null && \
           cargo build --release --bin capsule -p capsule-cli 2>/dev/null; then
            echo -e "${GREEN}✓${NC}"
        else
            echo -e "${RED}✗${NC}"
            exit 1
        fi
        
        echo ""
        echo -e "${GREEN}✅ All checks passed!${NC}"
        echo ""
        ;;
    s|S)
        echo ""
        echo -e "${YELLOW}⚠️  Skipping checks...${NC}"
        echo ""
        ;;
    *)
        echo ""
        echo -e "${RED}Push cancelled.${NC}"
        exit 1
        ;;
esac

# Push
echo -e "${BLUE}Pushing to origin...${NC}"
git push origin "$(git rev-parse --abbrev-ref HEAD)"

echo ""
echo -e "${GREEN}✅ Push complete!${NC}"
