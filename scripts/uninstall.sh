#!/bin/sh
#
# Fallback uninstaller for `curl ato.run/install.sh | sh` deployments.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/ato-run/ato/main/scripts/uninstall.sh | sh -s -- --purge
#   curl -fsSL https://ato.run/install.sh | sh -s -- --uninstall --purge   (when ato.run wires this)
#
# This script intentionally mirrors the high-level behavior of `ato uninstall`
# so users with a broken CLI can still clean up. If `ato` runs at all, prefer
# `ato uninstall` — it has better error reporting and path discovery.

set -eu

ATO_INSTALL_DIR="${ATO_INSTALL_DIR:-$HOME/.local/bin}"
ATO_HOME="${ATO_HOME:-$HOME/.ato}"
PURGE=0
INCLUDE_CONFIG=0
INCLUDE_KEYS=0
DRY_RUN=0
YES=0

REMOVED_BINARIES=0
REMOVED_COMPLETIONS=0
REMOVED_DESKTOP_INTEGRATION=0
REMOVED_DESKTOP_BUNDLE=0
REMOVED_STORE=0
REMOVED_RUNTIMES=0
REMOVED_RUN=0
REMOVED_RUNS=0
REMOVED_APP_SESSIONS=0
REMOVED_LOGS=0
REMOVED_TMP=0
REMOVED_CACHE=0
REMOVED_EXECUTIONS=0
REMOVED_EPHEMERAL=0
REMOVED_CONFIG=0
REMOVED_KEYS=0
REMOVED_ATO_HOME=0
FAILED=0

usage() {
    echo "Usage: uninstall.sh [--purge] [--include-config] [--include-keys] [--dry-run] [--yes]" >&2
    exit 2
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --purge)
            PURGE=1
            ;;
        --include-config)
            INCLUDE_CONFIG=1
            ;;
        --include-keys)
            INCLUDE_KEYS=1
            ;;
        --dry-run)
            DRY_RUN=1
            ;;
        -y|--yes)
            YES=1
            ;;
        -h|--help)
            usage
            ;;
        *)
            echo "Unknown flag: $1" >&2
            usage
            ;;
    esac
    shift
done

if [ "$PURGE" -ne 1 ] && { [ "$INCLUDE_CONFIG" -eq 1 ] || [ "$INCLUDE_KEYS" -eq 1 ]; }; then
    echo "--include-config and --include-keys require --purge" >&2
    exit 2
fi

if [ "$INCLUDE_KEYS" -eq 1 ]; then
    echo "WARNING: --include-keys will permanently remove ~/.ato/keys." >&2
    echo "You may lose signing keys, trust anchors, and encrypted credential access." >&2
    echo >&2
fi

mark_removed() {
    case "$1" in
        binaries) REMOVED_BINARIES=1 ;;
        completions) REMOVED_COMPLETIONS=1 ;;
        desktop-integration) REMOVED_DESKTOP_INTEGRATION=1 ;;
        desktop-bundle) REMOVED_DESKTOP_BUNDLE=1 ;;
        store) REMOVED_STORE=1 ;;
        runtimes) REMOVED_RUNTIMES=1 ;;
        run) REMOVED_RUN=1 ;;
        runs) REMOVED_RUNS=1 ;;
        app-sessions) REMOVED_APP_SESSIONS=1 ;;
        logs) REMOVED_LOGS=1 ;;
        tmp) REMOVED_TMP=1 ;;
        cache) REMOVED_CACHE=1 ;;
        executions) REMOVED_EXECUTIONS=1 ;;
        ephemeral) REMOVED_EPHEMERAL=1 ;;
        config) REMOVED_CONFIG=1 ;;
        keys) REMOVED_KEYS=1 ;;
        ato-home) REMOVED_ATO_HOME=1 ;;
    esac
}

target_exists() {
    [ -e "$1" ] || [ -L "$1" ]
}

remove_target() {
    path="$1"
    group="$2"
    if target_exists "$path"; then
        if [ "$DRY_RUN" -eq 1 ]; then
            echo "- [remove] $path"
        else
            if rm -rf "$path"; then
                mark_removed "$group"
            else
                echo "warning: failed to remove $path" >&2
                FAILED=1
            fi
        fi
    elif [ "$DRY_RUN" -eq 1 ]; then
        echo "- [skip  ] $path"
    fi
}

print_removed_summary() {
    [ "$REMOVED_BINARIES" -eq 1 ] && echo "- binaries and shims"
    [ "$REMOVED_COMPLETIONS" -eq 1 ] && echo "- shell completions"
    [ "$REMOVED_DESKTOP_INTEGRATION" -eq 1 ] && echo "- desktop integration / launchers"
    [ "$REMOVED_DESKTOP_BUNDLE" -eq 1 ] && echo "- desktop bundle"
    [ "$REMOVED_STORE" -eq 1 ] && echo "- ~/.ato/store"
    [ "$REMOVED_RUNTIMES" -eq 1 ] && echo "- ~/.ato/runtimes"
    [ "$REMOVED_RUN" -eq 1 ] && echo "- ~/.ato/run"
    [ "$REMOVED_RUNS" -eq 1 ] && echo "- ~/.ato/runs"
    [ "$REMOVED_APP_SESSIONS" -eq 1 ] && echo "- ~/.ato/apps/*/sessions"
    [ "$REMOVED_LOGS" -eq 1 ] && echo "- ~/.ato/logs"
    [ "$REMOVED_TMP" -eq 1 ] && echo "- ~/.ato/.tmp / ~/.ato/tmp"
    [ "$REMOVED_CACHE" -eq 1 ] && echo "- ~/.ato/cache"
    [ "$REMOVED_EXECUTIONS" -eq 1 ] && echo "- ~/.ato/executions"
    [ "$REMOVED_EPHEMERAL" -eq 1 ] && echo "- cache / lock / pid / socket files"
    [ "$REMOVED_CONFIG" -eq 1 ] && echo "- ~/.ato/config.toml"
    [ "$REMOVED_KEYS" -eq 1 ] && echo "- ~/.ato/keys"
    [ "$REMOVED_ATO_HOME" -eq 1 ] && echo "- ~/.ato"
}

print_preserved_summary() {
    if [ "$PURGE" -eq 1 ] && [ "$INCLUDE_CONFIG" -ne 1 ]; then
        echo "- ~/.ato/config.toml"
    fi
    if [ "$PURGE" -eq 1 ] && [ "$INCLUDE_KEYS" -ne 1 ]; then
        echo "- ~/.ato/keys"
    fi
}

confirm_purge() {
    if [ "$PURGE" -ne 1 ] || [ "$YES" -eq 1 ] || [ "$DRY_RUN" -eq 1 ]; then
        return 0
    fi

    echo "ato uninstall --purge will remove:" >&2
    echo "- binaries and shims" >&2
    echo "- regeneratable data under ~/.ato" >&2
    if [ "$INCLUDE_CONFIG" -eq 1 ]; then
        echo "- ~/.ato/config.toml" >&2
    fi
    if [ "$INCLUDE_KEYS" -eq 1 ]; then
        echo "- ~/.ato/keys" >&2
    fi
    echo >&2
    printf "Proceed with purge? [y/N] " >&2
    read answer || exit 1
    case "$answer" in
        y|Y|yes|YES)
            return 0
            ;;
        *)
            echo "Aborted." >&2
            exit 0
            ;;
    esac
}

# Refuse to touch Homebrew-managed paths.
case "$ATO_INSTALL_DIR" in
    /opt/homebrew/*|/opt/homebrew|/usr/local/Cellar/*|/usr/local/opt/*|/home/linuxbrew/*)
        echo "ATO_INSTALL_DIR=$ATO_INSTALL_DIR looks Homebrew-managed." >&2
        echo "Run instead:" >&2
        echo "    brew uninstall ato-cli" >&2
        echo "    brew uninstall --cask ato 2>/dev/null || true" >&2
        exit 1
        ;;
esac

if [ "$DRY_RUN" -eq 1 ]; then
    echo "Dry run — ato uninstall"
    echo
    echo "Would remove:"
fi

confirm_purge

remove_target "$ATO_INSTALL_DIR/ato" binaries
remove_target "$ATO_INSTALL_DIR/nacelle" binaries

case "$(uname -s)" in
    Darwin)
        remove_target "/Applications/Ato Desktop.app" desktop-bundle
        remove_target "$HOME/Applications/Ato Desktop.app" desktop-bundle
        remove_target "$HOME/Library/Application Support/Ato" desktop-integration
        remove_target "$HOME/Library/Caches/run.ato.desktop" desktop-integration
        remove_target "$HOME/Library/Logs/run.ato.desktop" desktop-integration
        remove_target "$HOME/Library/Preferences/run.ato.desktop.plist" desktop-integration
        ;;
    Linux)
        remove_target "$HOME/Applications/Ato-Desktop.AppImage" desktop-bundle
        ;;
esac

remove_target "$HOME/.local/share/bash-completion/completions/ato" completions
remove_target "$HOME/.zsh/completions/_ato" completions
remove_target "$HOME/.local/share/zsh/site-functions/_ato" completions
remove_target "$HOME/.config/fish/completions/ato.fish" completions
remove_target "$HOME/.local/share/applications/ato.desktop" desktop-integration
remove_target "$HOME/.local/share/applications/ato-desktop.desktop" desktop-integration
remove_target "$HOME/.local/share/icons/hicolor/512x512/apps/ato.png" desktop-integration
remove_target "$HOME/.local/share/icons/hicolor/512x512/apps/ato-desktop.png" desktop-integration

if [ "$PURGE" -eq 1 ]; then
    remove_target "$ATO_HOME/store" store
    remove_target "$ATO_HOME/runtimes" runtimes
    remove_target "$ATO_HOME/run" run
    remove_target "$ATO_HOME/runs" runs
    remove_target "$ATO_HOME/logs" logs
    remove_target "$ATO_HOME/.tmp" tmp
    remove_target "$ATO_HOME/tmp" tmp
    remove_target "$ATO_HOME/cache" cache
    remove_target "$ATO_HOME/executions" executions
    remove_target "$ATO_HOME/desktop" desktop-integration

    if [ -d "$ATO_HOME/apps" ]; then
        for app_root in "$ATO_HOME"/apps/*; do
            [ -d "$app_root" ] || continue
            remove_target "$app_root/sessions" app-sessions
        done
    fi

    for path in "$ATO_HOME"/*.lock "$ATO_HOME"/*.pid "$ATO_HOME"/*.sock "$ATO_HOME"/lock "$ATO_HOME"/pid "$ATO_HOME"/socket; do
        remove_target "$path" ephemeral
    done

    if [ "$INCLUDE_CONFIG" -eq 1 ]; then
        remove_target "$ATO_HOME/config.toml" config
    fi
    if [ "$INCLUDE_KEYS" -eq 1 ]; then
        remove_target "$ATO_HOME/keys" keys
    fi
fi

if [ "$DRY_RUN" -eq 1 ]; then
    if [ "$PURGE" -eq 1 ]; then
        echo
        echo "Would preserve:"
        print_preserved_summary
    fi
    exit 0
fi

if [ -d "$ATO_HOME" ] && [ -z "$(find "$ATO_HOME" -mindepth 1 -print -quit 2>/dev/null)" ]; then
    rmdir "$ATO_HOME" && mark_removed ato-home || true
fi

echo
if [ "$FAILED" -eq 0 ] && ! print_removed_summary | grep -q .; then
    echo "No matching installed files were found."
else
    echo "Removed:"
    print_removed_summary || true
fi

if [ "$PURGE" -eq 1 ]; then
    echo
    echo "Preserved:"
    print_preserved_summary
    if [ "$INCLUDE_CONFIG" -ne 1 ] || [ "$INCLUDE_KEYS" -ne 1 ]; then
        echo
        echo "Use --include-config and/or --include-keys for full removal."
    fi
fi

echo
echo "Remove $ATO_INSTALL_DIR from your PATH if you want a fully clean shell environment."

if [ "$FAILED" -ne 0 ]; then
    exit 1
fi
