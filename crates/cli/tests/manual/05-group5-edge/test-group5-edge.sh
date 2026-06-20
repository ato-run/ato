#!/bin/bash
# =============================================================================
# Group 5: エッジケース・セキュリティ
# ケース: 5a (CI env), 5b (dry-run secret scan), 5c (env injection denylist),
#         5d (non-secret key = no mask), 5e (.env* excluded from capsule)
# =============================================================================
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../config.sh"

RESULT_FILE="$RESULTS_DIR/result_group5.log"
: > "$RESULT_FILE"
log() { echo "$*" | tee -a "$RESULT_FILE"; }

PASSED=0; FAILED=0
FAILURES=()

pass() { ((PASSED++)); print_status "PASS" "$1"; log "[PASS] $1"; }
fail() { ((FAILED++)); FAILURES+=("$1: $2"); print_status "FAIL" "$1: $2"; log "[FAIL] $1: $2"; }

# ---------------------------------------------------------------------------
# 5a: CI環境 (GITHUB_ACTIONS=true) → プロンプトなし・env var直接参照
# ---------------------------------------------------------------------------
test_5a() {
    log "--- Test 5a: CI env (GITHUB_ACTIONS=true) → no prompt ---"
    local out="$RESULTS_DIR/5a_output.txt"

    GITHUB_ACTIONS=true OPENAI_API_KEY=sk-test-1234567890abcdef \
        gtimeout 30 ato run -y github.com/openai/openai-realtime-console >"$out" 2>&1 || true

    # Should NOT show masked input prompt
    if grep -qi "Enter value.*OPENAI_API_KEY\|OPENAI_API_KEY.*hidden" "$out"; then
        fail "5a" "Prompt appeared in CI mode. Output: $(cat "$out" | head -15)"
    else
        pass "5a"
    fi
}

# ---------------------------------------------------------------------------
# 5b: --dry-run でシークレット検出
# ---------------------------------------------------------------------------
setup_5b() {
    mkdir -p "$ATO_TEST_DIR/test-5b"
    cat > "$ATO_TEST_DIR/test-5b/config.env" <<'EOF'
OPENAI_API_KEY=sk-test-ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij
EOF
    echo '{"name":"test-5b","version":"0.0.1","private":true}' > "$ATO_TEST_DIR/test-5b/package.json"
}

test_5b() {
    log "--- Test 5b: ato encap --dry-run → secret detected ---"
    setup_5b

    local out="$RESULTS_DIR/5b_output.txt"
    gtimeout 15 ato encap --dry-run "$ATO_TEST_DIR/test-5b" >"$out" 2>&1 || true

    if grep -qi 'secret\|sk-\|potential\|found\|detect' "$out"; then
        pass "5b"
    else
        fail "5b" "Secret detection not reported. Output: $(cat "$out" | head -15)"
    fi
}

# ---------------------------------------------------------------------------
# 5c: env injection denylist (LD_PRELOAD, NODE_OPTIONS --require)
# ---------------------------------------------------------------------------
setup_5c() {
    echo 'LD_PRELOAD=/lib/malicious.so' > "$ATO_TEST_DIR/test-5c.env"
    echo 'NODE_OPTIONS=--require=/hook.js' > "$ATO_TEST_DIR/test-5c-nodeopts.env"
}

test_5c() {
    log "--- Test 5c: env injection denylist ---"
    setup_5c

    # LD_PRELOAD case
    local out_ld="$RESULTS_DIR/5c_ldpreload.txt"
    gtimeout 15 ato run --env-file "$ATO_TEST_DIR/test-5c.env" github.com/openai/openai-realtime-console >"$out_ld" 2>&1 || true

    # NODE_OPTIONS case
    local out_node="$RESULTS_DIR/5c_nodeopts.txt"
    gtimeout 15 ato run --env-file "$ATO_TEST_DIR/test-5c-nodeopts.env" github.com/openai/openai-realtime-console >"$out_node" 2>&1 || true

    local ld_ok=false
    local node_ok=false

    if grep -qi 'block\|denied\|rejected\|error.*LD_PRELOAD\|LD_PRELOAD.*block\|forbidden\|denylist\|security' "$out_ld"; then
        ld_ok=true
    fi
    if grep -qi 'block\|denied\|rejected\|error.*NODE_OPTIONS\|NODE_OPTIONS.*block\|dangerous\|forbidden\|denylist\|security\|--require' "$out_node"; then
        node_ok=true
    fi

    if $ld_ok && $node_ok; then
        pass "5c"
    elif $ld_ok; then
        fail "5c" "NODE_OPTIONS injection not blocked. Output: $(cat "$out_node" | head -10)"
    elif $node_ok; then
        fail "5c" "LD_PRELOAD not blocked. Output: $(cat "$out_ld" | head -10)"
    else
        fail "5c" "Neither LD_PRELOAD nor NODE_OPTIONS blocked. LD: $(cat "$out_ld" | head -5) NODE: $(cat "$out_node" | head -5)"
    fi
}

# ---------------------------------------------------------------------------
# 5d: 非シークレットキー → マスクなし入力
# ---------------------------------------------------------------------------
setup_5d() {
    mkdir -p "$ATO_TEST_DIR/test-5d"
    cat > "$ATO_TEST_DIR/test-5d/.env.example" <<'EOF'
PORT=
HOST=
EOF
    rm -f "$ATO_TEST_DIR/test-5d/.env"
    cat > "$ATO_TEST_DIR/test-5d/package.json" <<'EOF'
{
  "name": "test-5d",
  "version": "0.0.1",
  "private": true,
  "scripts": {
    "dev": "node -e \"console.log('PORT=' + process.env.PORT + ' HOST=' + process.env.HOST)\""
  }
}
EOF
}

test_5d() {
    log "--- Test 5d: non-secret keys → no mask ---"
    setup_5d

    local out="$RESULTS_DIR/5d_output.txt"
    # Send empty input
    printf "\n\n" | gtimeout 20 ato run "$ATO_TEST_DIR/test-5d" >"$out" 2>&1 || true

    # PORT and HOST prompts should appear WITHOUT "hidden" indicator
    if grep -qi 'PORT\|HOST' "$out"; then
        # Should NOT have "hidden" marker for these non-secret keys
        if grep -qi 'PORT.*hidden\|HOST.*hidden' "$out"; then
            fail "5d" "PORT/HOST marked as hidden (should be plain text). Output: $(cat "$out" | head -15)"
        else
            pass "5d"
        fi
    else
        fail "5d" "PORT/HOST prompt not found. Output: $(cat "$out" | head -15)"
    fi
}

# ---------------------------------------------------------------------------
# 5e: .env* が capsule から除外される確認 (dry-run)
# ---------------------------------------------------------------------------
setup_5e() {
    mkdir -p "$ATO_TEST_DIR/test-5e"
    cat > "$ATO_TEST_DIR/test-5e/.env" <<'EOF'
OPENAI_API_KEY=sk-test-ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij
EOF
    cat > "$ATO_TEST_DIR/test-5e/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "test-5e"
version = "0.0.1"
run = "node server.js"
runtime = "source/node"
EOF
    echo 'console.log("started")' > "$ATO_TEST_DIR/test-5e/server.js"
}

test_5e() {
    log "--- Test 5e: .env* excluded from capsule (dry-run) ---"
    setup_5e

    local out="$RESULTS_DIR/5e_output.txt"
    gtimeout 15 ato encap --dry-run "$ATO_TEST_DIR/test-5e" >"$out" 2>&1 || true

    # .env は PackFilter で除外されるため、secretスキャンにもかからない
    if grep -qi 'No secret\|no.*secret\|0 potential\|clean' "$out"; then
        pass "5e"
    elif ! grep -qi 'sk-\|secret.*found\|potential secret' "$out"; then
        # スキャンに引っかからなければ除外されている
        pass "5e"
    else
        fail "5e" ".env secret was detected (should be excluded). Output: $(cat "$out" | head -15)"
    fi
}

echo "=========================================="
echo " Group 5: Edge Cases & Security"
echo "=========================================="
check_ato

test_5a
test_5b
test_5c
test_5d
test_5e

echo ""
echo "--- Group 5 Results ---"
echo "Passed: $PASSED, Failed: $FAILED"
for f in "${FAILURES[@]}"; do echo "  FAIL: $f"; done
log "--- SUMMARY: PASSED=$PASSED FAILED=$FAILED ---"

[ $FAILED -eq 0 ] && exit 0 || exit 1
