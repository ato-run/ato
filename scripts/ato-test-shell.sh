#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TMP_ROOT="$REPO_ROOT/.tmp/ato-test-shell"
mkdir -p "$TMP_ROOT"

if [ "${ATO_TEST_REUSE_ENV_ROOT:-0}" = "1" ] && [ -n "${ATO_TEST_ENV_ROOT:-}" ]; then
    ENV_ROOT="$ATO_TEST_ENV_ROOT"
else
    ENV_ROOT="$(mktemp -d "$TMP_ROOT/env.XXXXXX")"
fi
export ATO_TEST_ENV_ROOT="$ENV_ROOT"
export ATO_HOME="$ENV_ROOT/ato-home"
export HOME="$ENV_ROOT/home"
export XDG_CONFIG_HOME="$ENV_ROOT/xdg-config"
export XDG_CACHE_HOME="$ENV_ROOT/xdg-cache"
unset ATO_DESKTOP_SESSION_ROOT
unset DESKY_SESSION_ROOT

mkdir -p "$ATO_HOME" "$HOME" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME"

print_env() {
    cat <<EOF
ATO_TEST_ENV_ROOT=$ATO_TEST_ENV_ROOT
ATO_HOME=$ATO_HOME
HOME=$HOME
XDG_CONFIG_HOME=$XDG_CONFIG_HOME
XDG_CACHE_HOME=$XDG_CACHE_HOME
EOF
}

if [ "${1:-}" = "--print-env" ]; then
    print_env
    shift
fi

if [ $# -eq 0 ]; then
    print_env
    exec "${SHELL:-/bin/bash}" -i
fi

exec "$@"
