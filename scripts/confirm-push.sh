#!/bin/bash
# =============================================================================
# Confirm-push hook for capsuled
# Asks for confirmation before running pre-push checks
# 
# Installation:
#   chmod +x scripts/confirm-push.sh
#   ln -sf ../../scripts/confirm-push.sh .git/hooks/pre-push
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

# Determine project root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ "$SCRIPT_DIR" == *".git/hooks"* ]]; then
    # Running as git hook - find project root from git
    PROJECT_ROOT="$(git rev-parse --show-toplevel)"
else
    # Running directly from scripts/
    PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
fi

cd "$PROJECT_ROOT"

echo ""
echo -e "${YELLOW}╔═══════════════════════════════════════════════════════════════╗${NC}"
echo -e "${YELLOW}║           CAPSULED PRE-PUSH CONFIRMATION                      ║${NC}"
echo -e "${YELLOW}╚═══════════════════════════════════════════════════════════════╝${NC}"
echo ""

# Show current status
echo -e "${BLUE}Current branch:${NC} $(git rev-parse --abbrev-ref HEAD)"
echo -e "${BLUE}Commits to push:${NC}"
git log --oneline origin/main..HEAD 2>/dev/null || echo "  (new branch or no upstream)"
echo ""

# Interactive confirmation
# - y: skip verification (assuming already done manually)
# - n: print steps and exit non-zero to cancel push
# Note: Git hooks don't have stdin connected to TTY, so we use /dev/tty
if [ -t 1 ] && exec < /dev/tty; then
    echo -e "${YELLOW}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${YELLOW}確認:${NC} すでに手動で pre-push 検証を実行しましたか？"
    echo -e "${BLUE}y${NC}: 実行済み（push を続行） / ${BLUE}n${NC}: 手順を表示して中断"
    echo -e "${YELLOW}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    read -r -p "Choice [y/n]: " choice
    
    case "$choice" in
        y|Y)
            echo ""
            echo -e "${GREEN}✅ OK: push を続行します${NC}"
            echo ""
            exit 0
            ;;
        n|N)
            echo ""
            echo -e "${YELLOW}Push cancelled.${NC} 先に以下を実行してください:"
            echo ""
            echo "  cd $PROJECT_ROOT"
            echo "  bash ./scripts/pre-push-hook.sh"
            echo ""
            echo -e "${BLUE}検証が成功したら push してください:${NC}"
            echo "  git push"
            echo ""
            exit 1
            ;;
        *)
            echo ""
            echo -e "${RED}Invalid choice. Push cancelled.${NC}"
            exit 1
            ;;
    esac
else
    # Non-interactive (CI or no TTY available) - always run checks
    echo -e "${BLUE}Non-interactive mode: running pre-push verification...${NC}"
    echo ""
    bash "$PROJECT_ROOT/scripts/pre-push-hook.sh"
fi
