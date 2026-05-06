#!/bin/bash
# Shared config and helpers for ato manual test suite
export RED='\033[0;31m'
export GREEN='\033[0;32m'
export YELLOW='\033[1;33m'
export BLUE='\033[0;34m'
export CYAN='\033[0;36m'
export NC='\033[0m'

export MANUAL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export RESULTS_DIR="$MANUAL_DIR/results"
mkdir -p "$RESULTS_DIR"

export ATO_TEST_TMP="$MANUAL_DIR/../../.tmp/manual-tests"
mkdir -p "$ATO_TEST_TMP"

setup_isolated_ato_env() {
    [ "${ATO_TEST_HERMETIC:-1}" = "1" ] || return 0

    if [ -z "${ATO_TEST_ENV_ROOT:-}" ]; then
        ATO_TEST_ENV_ROOT="$(mktemp -d "$ATO_TEST_TMP/env.XXXXXX")"
        export ATO_TEST_ENV_ROOT
    fi

    export ATO_HOME="${ATO_HOME:-$ATO_TEST_ENV_ROOT/ato-home}"
    export HOME="$ATO_TEST_ENV_ROOT/home"
    export XDG_CONFIG_HOME="${XDG_CONFIG_HOME:-$ATO_TEST_ENV_ROOT/xdg-config}"
    export XDG_CACHE_HOME="${XDG_CACHE_HOME:-$ATO_TEST_ENV_ROOT/xdg-cache}"
    unset ATO_DESKTOP_SESSION_ROOT
    unset DESKY_SESSION_ROOT

    mkdir -p "$ATO_HOME" "$HOME" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME"
}

ato_home_path() {
    if [ $# -eq 0 ]; then
        printf '%s\n' "$ATO_HOME"
    else
        printf '%s/%s\n' "$ATO_HOME" "$1"
    fi
}

setup_isolated_ato_env

# Per-suite result accumulators (set by each suite)
PASSED=0
FAILED=0
SKIPPED=0
FAILURES=()

print_status() {
    local status=$1 message=$2
    case $status in
        PASS) echo -e "${GREEN}[PASS]${NC} $message" ;;
        FAIL) echo -e "${RED}[FAIL]${NC} $message" ;;
        WARN) echo -e "${YELLOW}[WARN]${NC} $message" ;;
        INFO) echo -e "${BLUE}[INFO]${NC} $message" ;;
        SKIP) echo -e "${YELLOW}[SKIP]${NC} $message" ;;
    esac
}

pass()  { ((PASSED++));  print_status PASS "$1"; echo "[PASS] $1" >> "$RESULT_FILE"; }
fail()  { ((FAILED++));  FAILURES+=("$1: $2"); print_status FAIL "$1: $2"; echo "[FAIL] $1: $2" >> "$RESULT_FILE"; }
skip()  { ((SKIPPED++)); print_status SKIP "$1"; echo "[SKIP] $1" >> "$RESULT_FILE"; }
info()  { print_status INFO "$1"; echo "[INFO] $1" >> "$RESULT_FILE"; }

# Set ATO_TEST_AUTO=1 to auto-skip all human checks (for CI / automated runs)
: "${ATO_TEST_AUTO:=0}"

_tty_available() {
    [ "$ATO_TEST_AUTO" = "1" ] && return 1
    [ -e /dev/tty ] && return 0 || return 1
}

# Print a human checklist item and collect PASS/FAIL/SKIP from stdin
# Usage: human_check "description"
human_check() {
    local desc="$1"
    echo ""
    echo -e "${CYAN}━━━ HUMAN CHECK ━━━${NC}"
    echo -e "  $desc"
    if ! _tty_available; then
        skip "$desc (auto-skipped: non-interactive)"
        return
    fi
    echo -n "  Result? [p=pass / f=fail / s=skip]: "
    read -r ans < /dev/tty
    case "$ans" in
        p|P|pass) pass "$desc" ;;
        f|F|fail) fail "$desc" "Manual check failed by tester" ;;
        *)         skip "$desc" ;;
    esac
}

# Print a multi-line checklist for a test item
# Usage: checklist "Title" "step1" "step2" ...
checklist() {
    local title="$1"; shift
    echo ""
    echo -e "${CYAN}━━━ CHECKLIST: $title ━━━${NC}"
    local i=1
    for step in "$@"; do
        echo "  $i. $step"
        ((i++))
    done
    if ! _tty_available; then
        skip "$title (auto-skipped: non-interactive)"
        return
    fi
    echo -n "  All items pass? [p=pass / f=fail / s=skip]: "
    read -r ans < /dev/tty
    case "$ans" in
        p|P|pass) pass "$title" ;;
        f|F|fail) fail "$title" "One or more checklist items failed" ;;
        *)         skip "$title" ;;
    esac
}

check_ato() {
    if ! command -v ato &>/dev/null; then
        echo -e "${RED}Error: ato not found in PATH${NC}"
        echo "Build and install first: cd apps/ato-cli && cargo install --path ."
        exit 1
    fi
    info "ato version: $(ato --version 2>&1 | head -1)"
}

# Run a command with timeout; write stdout+stderr to outfile; return exit code
run_cmd() {
    local timeout=$1 outfile=$2; shift 2
    timeout "$timeout" "$@" >"$outfile" 2>&1
}

# Provision a Python capsule with pyproject.toml + uv.lock so `ato run` can proceed.
# Only adds files that don't already exist.
# Usage: provision_python_capsule <capsule_dir>
provision_python_capsule() {
    local dir="$1"
    local name
    name=$(basename "$dir" | LC_ALL=C tr -cs '[:alnum:]_-' '-' | sed 's/^-*//;s/-*$//')
    [ -z "$name" ] && name="test-capsule"
    if [ ! -f "$dir/pyproject.toml" ]; then
        cat > "$dir/pyproject.toml" <<PYEOF
[project]
name = "$name"
version = "0.1.0"
requires-python = ">=3.11"
dependencies = []
PYEOF
    fi
    if [ ! -f "$dir/uv.lock" ]; then
        ( cd "$dir" && uv lock --quiet 2>/dev/null ) || true
    fi
}

print_suite_summary() {
    local suite="$1"
    local hermetic="${ATO_TEST_HERMETIC:-1}"
    echo ""
    echo "══════════════════════════════════"
    echo " $suite — Results [hermetic=$hermetic]"
    echo "══════════════════════════════════"
    echo " PASS: $PASSED  FAIL: $FAILED  SKIP: $SKIPPED"
    for f in "${FAILURES[@]:-}"; do
        [ -n "$f" ] && echo "  ✗ $f"
    done
    echo ""
    [ "$FAILED" -eq 0 ] && return 0 || return 1
}
