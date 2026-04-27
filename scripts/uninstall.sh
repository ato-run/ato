#!/bin/sh
#
# Fallback uninstaller for `curl ato.run/install.sh | sh` deployments.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/ato-run/ato/main/scripts/uninstall.sh | sh
#   curl -fsSL https://ato.run/install.sh | sh -s -- --uninstall   (when ato.run wires this)
#
# This script intentionally duplicates the logic in `ato uninstall`
# so users with a broken CLI can still clean up. If `ato` runs at all,
# prefer `ato uninstall` — it has a confirmation prompt, --keep-data,
# and Homebrew-prefix detection.

set -eu

ATO_INSTALL_DIR="${ATO_INSTALL_DIR:-$HOME/.local/bin}"
KEEP_DATA="${ATO_UNINSTALL_KEEP_DATA:-0}"

remove() {
    if [ -e "$1" ] || [ -L "$1" ]; then
        rm -rf "$1" && echo "removed $1" || echo "warning: failed to remove $1"
    fi
}

# Refuse to touch Homebrew-managed paths.
case "$ATO_INSTALL_DIR" in
    /opt/homebrew/*|/opt/homebrew|/usr/local/Cellar/*|/usr/local/opt/*|/home/linuxbrew/*)
        echo "ATO_INSTALL_DIR=$ATO_INSTALL_DIR looks Homebrew-managed."
        echo "Run instead:"
        echo "    brew uninstall ato-cli"
        echo "    brew uninstall --cask ato 2>/dev/null || true"
        exit 1
        ;;
esac

echo "ato uninstall — removing files from $ATO_INSTALL_DIR and standard data dirs."

remove "$ATO_INSTALL_DIR/ato"
remove "$ATO_INSTALL_DIR/nacelle"

case "$(uname -s)" in
    Darwin)
        remove "/Applications/Ato Desktop.app"
        if [ "$KEEP_DATA" != "1" ]; then
            remove "$HOME/Library/Application Support/Ato"
            remove "$HOME/Library/Caches/run.ato.desktop"
            remove "$HOME/Library/Logs/run.ato.desktop"
            remove "$HOME/Library/Preferences/run.ato.desktop.plist"
        fi
        ;;
    Linux)
        remove "$HOME/Applications/Ato-Desktop.AppImage"
        ;;
esac

if [ "$KEEP_DATA" != "1" ]; then
    remove "$HOME/.ato/desktop"
fi

echo
echo "Done. Remove $ATO_INSTALL_DIR from your PATH (.zshrc / .bashrc) if you want a clean environment."
