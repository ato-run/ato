#!/bin/bash
# =============================================================================
# Group 2: Package Manager 検出 (B2機能)
# ケース: 2a (yarn), 2b (pnpm), 2c (bun), 2d (npm), 2e (yarn@4)
# Note: 実際に`npm install`等が走るので時間がかかる。出力からPM検出を確認。
# =============================================================================
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../config.sh"

RESULT_FILE="$RESULTS_DIR/result_group2.log"
: > "$RESULT_FILE"
log() { echo "$*" | tee -a "$RESULT_FILE"; }

PASSED=0; FAILED=0
FAILURES=()

pass() { ((PASSED++)); print_status "PASS" "$1"; log "[PASS] $1"; }
fail() { ((FAILED++)); FAILURES+=("$1: $2"); print_status "FAIL" "$1: $2"; log "[FAIL] $1: $2"; }

# PM検出テスト: ato run の出力から使われたPMを確認する
# ato run は install → dev/start の順で実行するので、
# "yarn install" / "pnpm install" / "bun install" / "npm install" などが出力に含まれるはず
test_pm() {
    local case_id="$1"
    local repo="$2"
    local expected_pm="$3"  # e.g. "yarn" "pnpm" "bun" "npm"
    local timeout_secs="${4:-60}"

    log "--- Test $case_id: $repo → expected PM: $expected_pm ---"
    local out="$RESULTS_DIR/${case_id}_output.txt"

    # Redirect stdin from /dev/null to prevent interactive prompts (rpassword, etc.)
    # We check output for PM name evidence (e.g. yarn.lock in preview, pnpm-lock.yaml message)
    gtimeout "$timeout_secs" ato run -y "$repo" >"$out" 2>&1 < /dev/null || true

    # Check if expected PM appears in output
    if grep -qi "$expected_pm" "$out"; then
        pass "$case_id"
    else
        # Also check for error about missing PM tool - still confirms detection
        if grep -qi "not found\|command not found" "$out" && grep -qi "$expected_pm" "$out"; then
            pass "$case_id (PM detected but not installed locally)"
        else
            fail "$case_id" "Expected '$expected_pm' in output. Got: $(cat "$out" | head -20)"
        fi
    fi
}

echo "=========================================="
echo " Group 2: Package Manager Detection"
echo "=========================================="
check_ato

# 2d は ✅ 済み (baseline) だが念のため再確認
print_status "INFO" "2d: npm (baseline) — confirmed ✅ in spec, running as sanity check"
test_pm "2d" "github.com/openai/openai-realtime-console" "npm" 60

# 2a: yarn (yarn.lock あり)
test_pm "2a" "github.com/excalidraw/excalidraw" "yarn" 90

# 2b: pnpm (pnpm-lock.yaml あり)
test_pm "2b" "github.com/elk-zone/elk" "pnpm" 90

# 2c: bun (bun.lockb あり)
test_pm "2c" "github.com/digitopvn/nextjs-bun-starter" "bun" 90

# 2e: yarn@4 (packageManager field) — brimdata/zui is ~233 MB; skip if download times out
test_2e_yarn4() {
    log "--- Test 2e: github.com/brimdata/zui → expected PM: yarn (v4) ---"
    local repo="github.com/brimdata/zui"
    local out="$RESULTS_DIR/2e_output.txt"

    gtimeout 90 ato run -y "$repo" >"$out" 2>&1 < /dev/null || true

    if [ ! -s "$out" ]; then
        print_status "WARN" "2e: brimdata/zui download timed out (repo is ~233MB; network too slow). SKIPPED."
        log "[WARN] 2e: skipped — download timeout (network)"
        return 0
    fi

    if grep -qi "yarn" "$out"; then
        pass "2e"
    else
        fail "2e" "Expected 'yarn' in output. Got: $(cat "$out" | head -20)"
    fi
}
test_2e_yarn4

echo ""
echo "--- Group 2 Results ---"
echo "Passed: $PASSED, Failed: $FAILED"
for f in "${FAILURES[@]}"; do echo "  FAIL: $f"; done
log "--- SUMMARY: PASSED=$PASSED FAILED=$FAILED ---"

[ $FAILED -eq 0 ] && exit 0 || exit 1
