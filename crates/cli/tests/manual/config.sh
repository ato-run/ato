#!/bin/bash
# Shared configuration for ato-cli manual tests
export RED='\033[0;31m'
export GREEN='\033[0;32m'
export YELLOW='\033[1;33m'
export BLUE='\033[0;34m'
export NC='\033[0m'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export SCRIPT_DIR
export RESULTS_DIR="$SCRIPT_DIR/results"
mkdir -p "$RESULTS_DIR"

export ATO_TEST_DIR="$HOME/ato-tests"

print_status() {
    local status=$1; local message=$2
    case $status in
        "PASS") echo -e "${GREEN}[PASS]${NC} $message" ;;
        "FAIL") echo -e "${RED}[FAIL]${NC} $message" ;;
        "WARN") echo -e "${YELLOW}[WARN]${NC} $message" ;;
        "INFO") echo -e "${BLUE}[INFO]${NC} $message" ;;
        "SKIP") echo -e "${YELLOW}[SKIP]${NC} $message" ;;
    esac
}

check_ato() {
    if ! command -v ato &>/dev/null; then
        echo "Error: ato not found in PATH"
        exit 1
    fi
    print_status "INFO" "ato version: $(ato --version 2>&1)"
}

# Run a command with timeout, capture output, return exit code
# Usage: run_with_timeout <timeout_seconds> <output_file> <cmd...>
run_with_timeout() {
    local timeout=$1
    local outfile=$2
    shift 2
    timeout "$timeout" "$@" >"$outfile" 2>&1
    return $?
}
