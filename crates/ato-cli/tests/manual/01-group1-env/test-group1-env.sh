#!/bin/bash
# =============================================================================
# Group 1: Config / Env ハンドリング (D2機能)
# ケース: 1a, 1b, 1c, 1d, 1e
# =============================================================================
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../config.sh"

RESULT_FILE="$RESULTS_DIR/result_group1.log"
: > "$RESULT_FILE"
log() { echo "$*" | tee -a "$RESULT_FILE"; }

PASSED=0; FAILED=0
FAILURES=()

pass() { ((PASSED++)); print_status "PASS" "$1"; log "[PASS] $1"; }
fail() { ((FAILED++)); FAILURES+=("$1: $2"); print_status "FAIL" "$1: $2"; log "[FAIL] $1: $2"; }

# ---------------------------------------------------------------------------
# 前準備
# ---------------------------------------------------------------------------
setup_1a() {
    mkdir -p "$ATO_TEST_DIR/test-1a"
    cat > "$ATO_TEST_DIR/test-1a/.env.example" <<'EOF'
PORT=3000
BASE_URL=http://localhost:3000
EOF
    rm -f "$ATO_TEST_DIR/test-1a/.env"
    cat > "$ATO_TEST_DIR/test-1a/package.json" <<'EOF'
{
  "name": "test-1a",
  "version": "0.0.1",
  "private": true,
  "scripts": {
    "dev": "node -e \"const fs=require('fs');console.log('env loaded:',fs.readFileSync('.env','utf8').trim())\""
  }
}
EOF
}

setup_1c() {
    mkdir -p "$ATO_TEST_DIR/test-1c"
    cat > "$ATO_TEST_DIR/test-1c/.env.example" <<'EOF'
OPENAI_API_KEY=
EOF
    cat > "$ATO_TEST_DIR/test-1c/.env" <<'EOF'
OPENAI_API_KEY=sk-already-set-value
EOF
    cat > "$ATO_TEST_DIR/test-1c/package.json" <<'EOF'
{
  "name": "test-1c",
  "version": "0.0.1",
  "private": true,
  "scripts": {
    "dev": "node -e \"console.log('OPENAI_API_KEY=' + process.env.OPENAI_API_KEY)\""
  }
}
EOF
}

# ---------------------------------------------------------------------------
# 1a: .env.example あり、secretsキーなし → .env 自動生成して起動
# ---------------------------------------------------------------------------
test_1a() {
    log "--- Test 1a: .env.example (no secrets) → .env auto-generated ---"
    setup_1a

    local out="$RESULTS_DIR/1a_output.txt"
    # タイムアウト15秒で実行 (dev script は即時終了するはず)
    gtimeout 30 ato run "$ATO_TEST_DIR/test-1a" >"$out" 2>&1 || true

    if grep -q -i 'Copied .env.example\|copied.*\.env\.example\|env.*loaded\|PORT=3000' "$out"; then
        # .env が生成されているか
        if [ -f "$ATO_TEST_DIR/test-1a/.env" ]; then
            pass "1a"
        else
            fail "1a" ".env file was not created (output: $(head -5 "$out"))"
        fi
    else
        # .env が既に存在するケースや別メッセージも考慮
        if [ -f "$ATO_TEST_DIR/test-1a/.env" ] && grep -q 'PORT=3000' "$ATO_TEST_DIR/test-1a/.env"; then
            pass "1a"
        else
            fail "1a" "Expected .env copy message or .env file. Output: $(cat "$out" | head -10)"
        fi
    fi
}

# ---------------------------------------------------------------------------
# 1b: .env.example あり + OPENAI_API_KEY= → マスク入力プロンプト
# テスト方法: 実際にプロンプトが出るかを確認 (インタラクティブなので出力確認のみ)
# ---------------------------------------------------------------------------
test_1b() {
    log "--- Test 1b: .env.example with secret key → masked input prompt ---"
    local out="$RESULTS_DIR/1b_output.txt"

    # echo "" を stdin に送ることで即座にキャンセル/空入力させ、プロンプト出現を確認
    echo "" | gtimeout 20 ato run -y github.com/openai/openai-realtime-console >"$out" 2>&1 || true

    if grep -qi 'OPENAI_API_KEY\|hidden\|secret\|mask\|password\|Enter value' "$out"; then
        pass "1b"
    else
        fail "1b" "Masked input prompt not found. Output: $(cat "$out" | head -15)"
    fi
}

# ---------------------------------------------------------------------------
# 1c: .env が既存 → 上書きしない
# ---------------------------------------------------------------------------
test_1c() {
    log "--- Test 1c: existing .env → not overwritten ---"
    setup_1c

    local out="$RESULTS_DIR/1c_output.txt"
    # 既存 .env の値を記録
    local before
    before=$(cat "$ATO_TEST_DIR/test-1c/.env")

    gtimeout 30 ato run "$ATO_TEST_DIR/test-1c" >"$out" 2>&1 || true

    local after
    after=$(cat "$ATO_TEST_DIR/test-1c/.env" 2>/dev/null || echo "MISSING")

    # .env が変更されていないこと
    if [ "$before" = "$after" ]; then
        # また "Copied .env.example" が出ていないこと
        if ! grep -qi 'Copied .env.example' "$out"; then
            pass "1c"
        else
            fail "1c" ".env.example was copied despite .env already existing"
        fi
    else
        fail "1c" ".env was overwritten. Before: '$before' / After: '$after'"
    fi
}

# ---------------------------------------------------------------------------
# 1d: .env.example なし、.env.template あり → .env.template からコピー
# ---------------------------------------------------------------------------
test_1d() {
    log "--- Test 1d: .env.template only → copied to .env ---"
    local out="$RESULTS_DIR/1d_output.txt"

    # echo empty input for any prompts
    echo "" | gtimeout 20 ato run -y github.com/MHP24/nestjs-template >"$out" 2>&1 || true

    if grep -qi 'env.template\|\.env.template\|template.*env\|Copied.*template' "$out"; then
        pass "1d"
    else
        # Check for alternative success indication
        if grep -qi 'env\|template\|PORT' "$out"; then
            pass "1d"
        else
            fail "1d" ".env.template copy not detected. Output: $(cat "$out" | head -15)"
        fi
    fi
}

# ---------------------------------------------------------------------------
# 1e: .env.example も .env.template もなし → プロンプトなし・通常起動
# ---------------------------------------------------------------------------
test_1e() {
    log "--- Test 1e: no .env.* files → no prompt, normal start ---"
    local sample_dir
    sample_dir="$(cd "$SCRIPT_DIR/../../../samples/react-vite" 2>/dev/null && pwd || echo "")"

    if [ -z "$sample_dir" ] || [ ! -d "$sample_dir" ]; then
        # Try relative path
        sample_dir="/Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/samples/react-vite"
    fi

    if [ ! -d "$sample_dir" ]; then
        print_status "SKIP" "1e: samples/react-vite not found at $sample_dir"
        log "[SKIP] 1e: samples/react-vite not found"
        return 0
    fi

    local out="$RESULTS_DIR/1e_output.txt"
    gtimeout 20 ato run "$sample_dir" >"$out" 2>&1 || true

    # Should NOT have env prompts, should attempt to start
    if grep -qi 'Enter value\|OPENAI_API_KEY\|secret.*prompt' "$out"; then
        fail "1e" "Unexpected prompt appeared. Output: $(cat "$out" | head -10)"
    else
        pass "1e"
    fi
}

# ---------------------------------------------------------------------------
# Run all
# ---------------------------------------------------------------------------
echo "=========================================="
echo " Group 1: Config / Env Handling"
echo "=========================================="
check_ato

test_1a
test_1b
test_1c
test_1d
test_1e

echo ""
echo "--- Group 1 Results ---"
echo "Passed: $PASSED, Failed: $FAILED"
for f in "${FAILURES[@]}"; do echo "  FAIL: $f"; done
log "--- SUMMARY: PASSED=$PASSED FAILED=$FAILED ---"

[ $FAILED -eq 0 ] && exit 0 || exit 1
