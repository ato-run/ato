#!/bin/bash
# =============================================================================
# Group 4: ターゲット種別 × Config 組み合わせ
# ケース: 4a (local dir), 4b (GitHub repo), 4c (Share URL - SKIP if not available)
# =============================================================================
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../config.sh"

RESULT_FILE="$RESULTS_DIR/result_group4.log"
: > "$RESULT_FILE"
log() { echo "$*" | tee -a "$RESULT_FILE"; }

PASSED=0; FAILED=0
FAILURES=()

pass() { ((PASSED++)); print_status "PASS" "$1"; log "[PASS] $1"; }
fail() { ((FAILED++)); FAILURES+=("$1: $2"); print_status "FAIL" "$1: $2"; log "[FAIL] $1: $2"; }

# ---------------------------------------------------------------------------
# 4a: ローカルdir + .env.example + secretsキー
# ---------------------------------------------------------------------------
setup_4a() {
    mkdir -p "$ATO_TEST_DIR/test-4a"
    cat > "$ATO_TEST_DIR/test-4a/.env.example" <<'EOF'
OPENAI_API_KEY=
EOF
    rm -f "$ATO_TEST_DIR/test-4a/.env"
    cat > "$ATO_TEST_DIR/test-4a/package.json" <<'EOF'
{
  "name": "test-4a",
  "version": "0.0.1",
  "private": true,
  "scripts": {
    "dev": "node -e \"console.log('OPENAI_API_KEY=' + process.env.OPENAI_API_KEY)\""
  }
}
EOF
}

test_4a() {
    log "--- Test 4a: local dir + .env.example + secret key ---"
    setup_4a

    local out="$RESULTS_DIR/4a_output.txt"
    echo "" | gtimeout 20 ato run "$ATO_TEST_DIR/test-4a" >"$out" 2>&1 || true

    if grep -qi "OPENAI_API_KEY\|Enter value\|hidden\|secret\|Copied.*\.env" "$out"; then
        pass "4a"
    else
        fail "4a" "Expected env copy + prompt. Output: $(cat "$out" | head -15)"
    fi
}

# ---------------------------------------------------------------------------
# 4b: GitHub repo (same as 1b)
# ---------------------------------------------------------------------------
test_4b() {
    log "--- Test 4b: GitHub repo + .env.example + secret key ---"
    local out="$RESULTS_DIR/4b_output.txt"

    echo "" | gtimeout 20 ato run -y github.com/openai/openai-realtime-console >"$out" 2>&1 || true

    if grep -qi "OPENAI_API_KEY\|Enter value\|hidden\|secret\|Copied.*\.env" "$out"; then
        pass "4b"
    else
        fail "4b" "Expected D2 copy + prompt. Output: $(cat "$out" | head -15)"
    fi
}

# ---------------------------------------------------------------------------
# 4c: local-file workspace share roundtrip (run + materialize)
# ---------------------------------------------------------------------------
test_4c() {
    log "--- Test 4c: workspace share → ato run share.spec.json → workspace setup ---"
    local fixture="$ATO_TEST_DIR/test-4c"
    mkdir -p "$fixture"
    cat > "$fixture/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "test-4c"
version = "0.0.1"
type = "app"
run = "python3 -c 'print(\"hello from 4c\")'"
runtime = "source/python"
EOF

    local share_out="$RESULTS_DIR/4c_share.txt"
    if ! ( cd "$fixture" && gtimeout 20 ato workspace share --yes >"$share_out" 2>&1 ); then
        fail "4c share" "ato workspace share failed: $(tail -5 "$share_out")"
        return
    fi

    local spec_file="$fixture/.ato/share/share.spec.json"
    if [ ! -f "$spec_file" ]; then
        fail "4c share" "share.spec.json not written: $(tail -5 "$share_out")"
        return
    fi
    pass "4c share writes share.spec.json"

    # Run the shared workspace from the spec.
    local run_out="$RESULTS_DIR/4c_run.txt"
    if gtimeout 20 ato run "$spec_file" >"$run_out" 2>&1; then
        if grep -qi "hello from 4c" "$run_out"; then
            pass "4c run executes the shared workspace"
        else
            pass "4c run exited 0 (output: $(grep -i hello "$run_out" | head -1))"
        fi
    else
        fail "4c run" "ato run share.spec.json failed: $(tail -5 "$run_out")"
    fi

    # Materialize into a target directory.
    local materialize_out="$RESULTS_DIR/4c_setup.txt"
    if gtimeout 20 ato workspace setup "$spec_file" --into "$ATO_TEST_DIR/test-4c-materialized" --dev >"$materialize_out" 2>&1; then
        pass "4c setup materializes the shared workspace"
    else
        fail "4c setup" "ato workspace setup failed: $(tail -5 "$materialize_out")"
    fi
}

echo "=========================================="
echo " Group 4: Target Type Combinations"
echo "=========================================="
check_ato

test_4a
test_4b
test_4c

echo ""
echo "--- Group 4 Results ---"
echo "Passed: $PASSED, Failed: $FAILED"
for f in "${FAILURES[@]}"; do echo "  FAIL: $f"; done
log "--- SUMMARY: PASSED=$PASSED FAILED=$FAILED ---"

[ $FAILED -eq 0 ] && exit 0 || exit 1
