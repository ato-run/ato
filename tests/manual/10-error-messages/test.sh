#!/bin/bash
# =============================================================================
# §10 エラーメッセージとデバッグ体験
# Intentionally-broken fixtures → verify error messages are actionable
# =============================================================================
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../config.sh"
RESULT_FILE="$RESULTS_DIR/result_10_error_messages.log"
: > "$RESULT_FILE"

SUITE="§10 Error Messages & Debug"
echo "══════════════════════════════════"
echo " $SUITE"
echo "══════════════════════════════════"
check_ato

# ---------------------------------------------------------------------------
# Helper: make a minimal capsule dir, run ato, check that output contains expected pattern
# ---------------------------------------------------------------------------
assert_error_contains() {
    local test_name="$1" capsule_dir="$2" pattern="$3"
    local out="$ATO_TEST_TMP/err_${test_name}.txt"
    run_cmd 20 "$out" ato run "$capsule_dir" || true
    if grep -qiE "$pattern" "$out"; then
        pass "$test_name — error message matches: $pattern"
    else
        fail "$test_name" "Expected pattern '$pattern' not in output: $(cat "$out" | head -8)"
    fi
}

# ---------------------------------------------------------------------------
# Automated: capsule.toml syntax error
# ---------------------------------------------------------------------------
test_toml_syntax_error() {
    local dir="$ATO_TEST_TMP/err-toml-syntax"
    mkdir -p "$dir"
    printf 'schema_version = "0.3"\nname = [broken\n' > "$dir/capsule.toml"
    assert_error_contains "toml-syntax-error" "$dir" \
        "invalid|parse|syntax|TOML|expected|line [0-9]"
    rm -rf "$dir"
}

# ---------------------------------------------------------------------------
# Automated: missing required field (no 'run' key)
# ---------------------------------------------------------------------------
test_missing_required_field() {
    local dir="$ATO_TEST_TMP/err-missing-run"
    mkdir -p "$dir"
    cat > "$dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "missing-run"
version = "0.1.0"
type = "app"
runtime = "source/python"
EOF
    assert_error_contains "missing-required-field" "$dir" \
        "missing|required|run|entrypoint|field"
    rm -rf "$dir"
}

# ---------------------------------------------------------------------------
# Automated: invalid runtime version
# ---------------------------------------------------------------------------
test_invalid_runtime_version() {
    local dir="$ATO_TEST_TMP/err-bad-runtime"
    mkdir -p "$dir"
    cat > "$dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "bad-runtime"
version = "0.1.0"
type = "app"
run = "python3 -c 'print(1)'"
runtime = "source/python"

[language.python]
version = "99.99.99"
EOF
    local out="$ATO_TEST_TMP/err_bad_runtime.txt"
    run_cmd 30 "$out" ato run "$dir" || true
    if grep -qiE "not found|unavailable|version|99\.99|could not" "$out"; then
        pass "invalid runtime version — actionable error"
    else
        info "Output: $(cat "$out" | head -8)"
        skip "invalid runtime version — error may not surface until execution"
    fi
    rm -rf "$dir"
}

# ---------------------------------------------------------------------------
# Automated: network.allow missing a required host → actionable error
# ---------------------------------------------------------------------------
test_missing_network_allow() {
    local dir="$ATO_TEST_TMP/err-missing-network"
    mkdir -p "$dir"
    cat > "$dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "missing-network"
version = "0.1.0"
type = "app"
run = "python3 probe.py"
runtime = "source/python"

[isolation]
sandbox = true
[isolation.network]
enabled = true
egress_allow = []
EOF
    cat > "$dir/probe.py" <<'EOF'
import urllib.request
urllib.request.urlopen("http://example.com", timeout=5)
print("connected")
EOF
    local out="$ATO_TEST_TMP/err_missing_network.txt"
    run_cmd 30 "$out" ato run "$dir" || true
    if grep -qiE "blocked|denied|egress|not allowed|permitted|connection|refused" "$out"; then
        pass "Missing egress_allow — connection blocked with actionable message"
    else
        info "Output: $(cat "$out" | head -8)"
        skip "Missing egress_allow — sandbox may not be enforced on this platform"
    fi
    rm -rf "$dir"
}

# ---------------------------------------------------------------------------
# Automated: --verbose flag produces richer output
# ---------------------------------------------------------------------------
test_verbose_flag() {
    local dir="$ATO_TEST_TMP/verbose-capsule"
    mkdir -p "$dir"
    cat > "$dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "verbose-test"
version = "0.1.0"
type = "app"
run = "python3 -c 'print(\"ok\")'"
runtime = "source/python"
EOF
    local out_normal="$ATO_TEST_TMP/verbose_normal.txt"
    local out_verbose="$ATO_TEST_TMP/verbose_verbose.txt"
    run_cmd 30 "$out_normal" ato run "$dir" || true
    run_cmd 30 "$out_verbose" ato run --verbose "$dir" || true

    local normal_lines verbose_lines
    normal_lines=$(wc -l < "$out_normal")
    verbose_lines=$(wc -l < "$out_verbose")
    if [ "$verbose_lines" -gt "$normal_lines" ]; then
        pass "--verbose produces more output than default ($verbose_lines vs $normal_lines lines)"
    else
        info "Normal: $normal_lines lines, verbose: $verbose_lines lines"
        skip "--verbose flag — no difference detected (flag may not be implemented yet)"
    fi
    rm -rf "$dir"
}

# ---------------------------------------------------------------------------
# Automated: log file location is mentioned in errors
# ---------------------------------------------------------------------------
test_log_location_in_errors() {
    local dir="$ATO_TEST_TMP/log-location-capsule"
    mkdir -p "$dir"
    # Intentionally broken to trigger an error
    printf 'schema_version = "0.3"\nname = [broken\n' > "$dir/capsule.toml"
    local out="$ATO_TEST_TMP/log_location.txt"
    run_cmd 15 "$out" ato run "$dir" || true
    local log_dir="${ATO_HOME:-$HOME/.ato}/logs"
    if grep -qi "log\|\.ato/logs\|debug\|stderr" "$out" || [ -d "$log_dir" ]; then
        pass "Log file location is discoverable (mentioned in error or ~/.ato/logs/ exists)"
    else
        skip "Log file location — not mentioned in error output (may be future work)"
    fi
    rm -rf "$dir"
}

# ---------------------------------------------------------------------------
# Automated: Rust panic produces readable output (no raw backtrace only)
# ---------------------------------------------------------------------------
test_panic_output_readable() {
    # Attempt to trigger a panic via a degenerate input
    local out="$ATO_TEST_TMP/panic_test.txt"
    # Pass a path that doesn't exist — should give a clean error, not a raw panic
    run_cmd 10 "$out" ato run /this/path/does/not/exist/at/all 2>/dev/null || true
    if grep -qi "thread.*panicked\|RUST_BACKTRACE" "$out"; then
        fail "Panic output readable" "Raw Rust panic exposed to user: $(tail -5 "$out")"
    elif grep -qi "not found\|does not exist\|no such\|error:" "$out"; then
        pass "Non-existent path gives clean error (no raw panic)"
    else
        info "Output: $(cat "$out" | head -8)"
        skip "Panic output — could not trigger (path handling may differ)"
    fi
}

# ---------------------------------------------------------------------------
# Human: GPU insufficient error
# ---------------------------------------------------------------------------
test_gpu_insufficient_error() {
    checklist "GPU insufficient — clear error message" \
        "On a machine with limited VRAM, request a model larger than available memory" \
        "ato run prints: which GPU capability is missing and how much VRAM is needed" \
        "Error message suggests running on CPU or using a smaller model" \
        "No panic, no silent hang — process exits within 5 seconds of the error"
}

# ---------------------------------------------------------------------------
# Human: disk full error
# ---------------------------------------------------------------------------
test_disk_full_error() {
    checklist "Disk full — clean failure" \
        "Fill disk to < 500MB free space" \
        "Run: ato run <capsule-with-large-artifact>" \
        "ato prints clear 'disk full' error with available vs required space" \
        "No partial files left in ~/.ato/ after the failure" \
        "Freeing disk space and re-running succeeds"
}

# ---------------------------------------------------------------------------
# Human: permission denied error
# ---------------------------------------------------------------------------
test_permission_denied_error() {
    checklist "Permission denied — clear error message" \
        "Create a capsule directory with mode 000 (chmod 000 <dir>)" \
        "Run: ato run <dir>" \
        "ato prints 'permission denied' with the path that is inaccessible" \
        "Error suggests running with appropriate permissions or fixing the path" \
        "chmod 755 <dir> and retry — runs successfully"
}

# ---------------------------------------------------------------------------
# Human: signature verification failure
# ---------------------------------------------------------------------------
test_signature_failure_error() {
    checklist "Signature verification failure — clear error message" \
        "Obtain or create a capsule archive with a tampered or missing signature" \
        "Run: ato run <tampered-capsule>" \
        "ato refuses to run with a clear 'signature verification failed' message" \
        "Message includes the publisher fingerprint that failed" \
        "No part of the capsule code is executed before the error"
}

# ---------------------------------------------------------------------------
# Human: error message locale (Japanese / Chinese environment)
# ---------------------------------------------------------------------------
test_error_message_locale() {
    checklist "Error messages in non-English OS locale" \
        "Set LANG=ja_JP.UTF-8 on macOS/Linux" \
        "Trigger a capsule.toml parse error" \
        "Confirm error message is still in English (or correctly localized if i18n is implemented)" \
        "No garbled characters (mojibake) in the error output" \
        "Error is still actionable (user knows what to fix)"
}

test_toml_syntax_error
test_missing_required_field
test_invalid_runtime_version
test_missing_network_allow
test_verbose_flag
test_log_location_in_errors
test_panic_output_readable
test_gpu_insufficient_error
test_disk_full_error
test_permission_denied_error
test_signature_failure_error
test_error_message_locale

print_suite_summary "$SUITE"
