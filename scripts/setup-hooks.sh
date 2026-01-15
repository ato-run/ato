#!/bin/bash
# =============================================================================
# Git Hooks Setup Script
# Installs pre-commit and pre-push hooks for nacelle
# =============================================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
GIT_HOOKS_DIR="$PROJECT_ROOT/.git/hooks"

echo "Setting up git hooks for nacelle..."

# Ensure hooks directory exists
mkdir -p "$GIT_HOOKS_DIR"

# Install pre-commit hook
if [ -f "$GIT_HOOKS_DIR/pre-commit" ]; then
    echo "Backing up existing pre-commit hook..."
    mv "$GIT_HOOKS_DIR/pre-commit" "$GIT_HOOKS_DIR/pre-commit.bak"
fi
ln -sf "$SCRIPT_DIR/pre-commit-hook.sh" "$GIT_HOOKS_DIR/pre-commit"
chmod +x "$SCRIPT_DIR/pre-commit-hook.sh"
echo "✓ Installed pre-commit hook (format + clippy)"

# Install pre-push hook
if [ -f "$GIT_HOOKS_DIR/pre-push" ]; then
    echo "Backing up existing pre-push hook..."
    mv "$GIT_HOOKS_DIR/pre-push" "$GIT_HOOKS_DIR/pre-push.bak"
fi
ln -sf "$SCRIPT_DIR/pre-push-hook.sh" "$GIT_HOOKS_DIR/pre-push"
chmod +x "$SCRIPT_DIR/pre-push-hook.sh"
echo "✓ Installed pre-push hook (format + clippy + tests + release build)"

echo ""
echo "✅ Git hooks installed successfully!"
echo ""
echo "Installed hooks:"
echo "  - pre-push: Runs format check, clippy, tests before pushing to main"
echo ""
echo "To bypass hooks (not recommended):"
echo "  git push --no-verify"
