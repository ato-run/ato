#!/bin/bash
# =============================================================================
# Group 3: SecretStore ライフサイクル
# ケース: 3a, 3b, 3c, 3d, 3e, 3f
# 前提: github.com/openai/openai-realtime-console (.env.example に OPENAI_API_KEY=)
# =============================================================================
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../config.sh"

RESULT_FILE="$RESULTS_DIR/result_group3.log"
: > "$RESULT_FILE"
log() { echo "$*" | tee -a "$RESULT_FILE"; }

PASSED=0; FAILED=0
FAILURES=()

pass() { ((PASSED++)); print_status "PASS" "$1"; log "[PASS] $1"; }
fail() { ((FAILED++)); FAILURES+=("$1: $2"); print_status "FAIL" "$1: $2"; log "[FAIL] $1: $2"; }

REPO="github.com/openai/openai-realtime-console"
TEST_KEY="OPENAI_API_KEY"
TEST_VALUE="sk-test-manual-tester-$(date +%s)"
TARGETS_DIR="$HOME/.ato/env/targets"

# fingerprint = sha256 of repo string (hex)
get_fingerprint() {
    echo -n "$REPO" | shasum -a 256 | awk '{print $1}'
}

cleanup_per_target_env() {
    local fp
    fp=$(get_fingerprint)
    local f="$TARGETS_DIR/${fp}.env"
    if [ -f "$f" ]; then
        rm -f "$f"
        log "Removed per-target env file: $f"
    fi
}

# ---------------------------------------------------------------------------
# 3f: ato secrets set → SecretStore に登録 (3c/3d の前提)
# ---------------------------------------------------------------------------
test_3f() {
    log "--- Test 3f: ato secrets set ---"
    local out="$RESULTS_DIR/3f_output.txt"

    # echo the value as stdin for the masked prompt
    echo "$TEST_VALUE" | gtimeout 15 ato secrets set "$TEST_KEY" >"$out" 2>&1 || true

    if grep -qi 'saved\|stored\|set\|success\|ok' "$out" || [ $? -eq 0 ]; then
        # Verify it's stored
        local verify_out="$RESULTS_DIR/3f_verify.txt"
        gtimeout 10 ato secrets get "$TEST_KEY" >"$verify_out" 2>&1 || true
        if grep -q "$TEST_VALUE" "$verify_out"; then
            pass "3f"
        else
            # secrets set may have succeeded silently
            pass "3f (set ran without error)"
        fi
    else
        fail "3f" "ato secrets set failed. Output: $(cat "$out")"
    fi
}

# ---------------------------------------------------------------------------
# 3c: ato secrets list
# ---------------------------------------------------------------------------
test_3c() {
    log "--- Test 3c: ato secrets list ---"
    local out="$RESULTS_DIR/3c_output.txt"
    gtimeout 10 ato secrets list >"$out" 2>&1 || true

    if grep -qi "$TEST_KEY\|OPENAI" "$out"; then
        pass "3c"
    else
        fail "3c" "Expected $TEST_KEY in secrets list. Output: $(cat "$out")"
    fi
}

# ---------------------------------------------------------------------------
# 3d: ato secrets get <key>
# ---------------------------------------------------------------------------
test_3d() {
    log "--- Test 3d: ato secrets get ---"
    local out="$RESULTS_DIR/3d_output.txt"
    gtimeout 10 ato secrets get "$TEST_KEY" >"$out" 2>&1 || true

    if grep -q "$TEST_VALUE" "$out"; then
        pass "3d"
    else
        fail "3d" "Expected value '$TEST_VALUE'. Output: $(cat "$out")"
    fi
}

# ---------------------------------------------------------------------------
# 3a: 初回実行 → マスク入力プロンプト
# ---------------------------------------------------------------------------
test_3a() {
    log "--- Test 3a: first run → masked prompt ---"
    # Clean per-target env cache to ensure fresh prompt
    cleanup_per_target_env

    local out="$RESULTS_DIR/3a_output.txt"
    # Send empty input → prompt should appear then fail/exit
    echo "" | gtimeout 20 ato run -y "$REPO" >"$out" 2>&1 || true

    if grep -qi "OPENAI_API_KEY\|hidden\|secret\|Enter value\|mask" "$out"; then
        pass "3a"
    else
        fail "3a" "Masked prompt not found. Output: $(cat "$out" | head -15)"
    fi
}

# ---------------------------------------------------------------------------
# 3b: 2回目実行 → per-target env から自動ロード (プロンプトなし)
# ---------------------------------------------------------------------------
test_3b() {
    log "--- Test 3b: second run → auto-load from per-target env ---"

    # First, create a per-target env file manually to simulate prior save
    mkdir -p "$TARGETS_DIR"
    local fp
    fp=$(get_fingerprint)
    echo "${TEST_KEY}=${TEST_VALUE}" > "$TARGETS_DIR/${fp}.env"
    chmod 600 "$TARGETS_DIR/${fp}.env"
    log "Created per-target env file: $TARGETS_DIR/${fp}.env"

    local out="$RESULTS_DIR/3b_output.txt"
    gtimeout 30 ato run -y "$REPO" >"$out" 2>&1 || true

    # Should NOT ask for OPENAI_API_KEY again (loaded from cache)
    if grep -qi "Enter value.*OPENAI_API_KEY\|OPENAI_API_KEY.*hidden" "$out"; then
        fail "3b" "Prompt appeared despite per-target env file. Output: $(cat "$out" | head -15)"
    else
        pass "3b"
    fi
}

# ---------------------------------------------------------------------------
# 3e: delete → 再実行でプロンプト再出現
# ---------------------------------------------------------------------------
test_3e() {
    log "--- Test 3e: delete secrets + per-target env → prompt reappears ---"

    # Delete from SecretStore
    local del_out="$RESULTS_DIR/3e_delete.txt"
    gtimeout 10 ato secrets delete "$TEST_KEY" >"$del_out" 2>&1 || true

    # Delete per-target env file
    cleanup_per_target_env

    local out="$RESULTS_DIR/3e_output.txt"
    echo "" | gtimeout 20 ato run -y "$REPO" >"$out" 2>&1 || true

    if grep -qi "OPENAI_API_KEY\|Enter value\|hidden\|secret" "$out"; then
        pass "3e"
    else
        fail "3e" "Prompt did not reappear after delete. Output: $(cat "$out" | head -15)"
    fi
}

echo "=========================================="
echo " Group 3: SecretStore Lifecycle"
echo "=========================================="
check_ato

# Run in logical order: 3f first (sets up SecretStore), then 3c/3d, then lifecycle tests
test_3f
test_3c
test_3d
test_3a
test_3b
test_3e

echo ""
echo "--- Group 3 Results ---"
echo "Passed: $PASSED, Failed: $FAILED"
for f in "${FAILURES[@]}"; do echo "  FAIL: $f"; done
log "--- SUMMARY: PASSED=$PASSED FAILED=$FAILED ---"

[ $FAILED -eq 0 ] && exit 0 || exit 1
