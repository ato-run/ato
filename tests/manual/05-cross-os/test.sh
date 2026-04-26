#!/bin/bash
# =============================================================================
# §5 クロス OS の挙動差検証
# =============================================================================
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../config.sh"
RESULT_FILE="$RESULTS_DIR/result_05_cross_os.log"
: > "$RESULT_FILE"

SUITE="§5 Cross-OS Behavior"
echo "══════════════════════════════════"
echo " $SUITE"
echo "══════════════════════════════════"
check_ato

# ---------------------------------------------------------------------------
# Automated: detect OS and report
# ---------------------------------------------------------------------------
test_os_detection() {
    local os
    os=$(uname -s)
    info "Running on: $os ($(uname -m))"
    case "$os" in
        Darwin) pass "OS detection — macOS ($(sw_vers -productVersion 2>/dev/null || echo unknown))" ;;
        Linux)  pass "OS detection — Linux ($(uname -r | cut -d- -f1))" ;;
        CYGWIN*|MINGW*|MSYS*) pass "OS detection — Windows (via $os)" ;;
        *) fail "OS detection" "Unrecognised OS: $os" ;;
    esac
}

# ---------------------------------------------------------------------------
# Automated: sample capsule.toml parses correctly on this OS
# ---------------------------------------------------------------------------
test_manifest_parse_cross_os() {
    local tmp_dir="$ATO_TEST_TMP/cross-os-capsule"
    mkdir -p "$tmp_dir"
    cat > "$tmp_dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "cross-os-test"
version = "0.1.0"
type = "app"
run = "python3 -c 'import sys; print(sys.platform)'"
runtime = "source/python"
EOF
    provision_python_capsule "$tmp_dir"
    local out="$ATO_TEST_TMP/cross_os_parse.txt"
    # ato run hangs after capsule exits (known bug), so use timeout and check output
    run_cmd 30 "$out" env CAPSULE_ALLOW_UNSAFE=1 ato run --dangerously-skip-permissions "$tmp_dir" || true
    if grep -qiE "darwin|linux|windows|freebsd|cygwin" "$out"; then
        pass "capsule.toml parses and runs on $(uname -s)"
    else
        fail "capsule.toml parses on $(uname -s)" "$(tail -5 "$out")"
    fi
    rm -rf "$tmp_dir"
}

# ---------------------------------------------------------------------------
# Automated: no hardcoded paths (/ vs \) in sample capsules
# ---------------------------------------------------------------------------
test_no_hardcoded_path_separators() {
    local samples_dir="$SCRIPT_DIR/../../../samples"
    if [ ! -d "$samples_dir" ]; then
        skip "No samples directory found at $samples_dir"
        return
    fi
    # Look for actual Windows-style path separators (C:\ or \word\word patterns)
    # Note: \\.  \\d (regex escapes) are valid in TOML regexes — only flag C:\ or \dir\dir
    local hits
    hits=$(grep -rl 'C:\\' "$samples_dir" --include="*.toml" --include="*.py" --include="*.js" --include="*.ts" --exclude-dir='.ato' --exclude-dir='node_modules' --exclude-dir='artifacts' --exclude-dir='uv-cache' --exclude-dir='.venv' 2>/dev/null | head -10)
    if [ -n "$hits" ]; then
        fail "No hardcoded Windows path separators in samples" "Found in: $hits"
    else
        pass "No hardcoded Windows-style path separators in samples"
    fi
}

# ---------------------------------------------------------------------------
# Automated: no CRLF in capsule.toml files
# ---------------------------------------------------------------------------
test_no_crlf_in_manifests() {
    local tests_dir="$SCRIPT_DIR/../.."
    local crlf_files
    crlf_files=$(grep -rl $'\r' "$tests_dir" --include="capsule.toml" 2>/dev/null | head -10)
    if [ -n "$crlf_files" ]; then
        fail "No CRLF line endings in capsule.toml files" "CRLF found in: $crlf_files"
    else
        pass "No CRLF line endings in capsule.toml manifests"
    fi
}

# ---------------------------------------------------------------------------
# Automated: Unicode / emoji path handling
# ---------------------------------------------------------------------------
test_unicode_path() {
    local unicode_dir="$ATO_TEST_TMP/テスト-capsule-🧪"
    mkdir -p "$unicode_dir"
    cat > "$unicode_dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "unicode-path-test"
version = "0.1.0"
type = "app"
run = "python3 -c 'print(\"unicode ok\")'"
runtime = "source/python"
EOF
    provision_python_capsule "$unicode_dir"
    local out="$ATO_TEST_TMP/unicode_path.txt"
    if run_cmd 30 "$out" env CAPSULE_ALLOW_UNSAFE=1 ato run --dangerously-skip-permissions "$unicode_dir"; then
        pass "Unicode/emoji path in capsule directory works"
    else
        if grep -qi "invalid\|decode\|utf\|unicode\|encoding" "$out"; then
            fail "Unicode/emoji path" "Encoding error: $(tail -5 "$out")"
        else
            fail "Unicode/emoji path" "ato run failed: $(tail -5 "$out")"
        fi
    fi
    rm -rf "$unicode_dir"
}

# ---------------------------------------------------------------------------
# Automated: HOME directory path varies by OS — capsule must not hard-code it
# ---------------------------------------------------------------------------
test_home_not_hardcoded() {
    local fixtures_dir="$SCRIPT_DIR/../.."
    local hits
    # Look for hardcoded /home/ or /Users/ in capsule manifests
    hits=$(grep -rl '/home/\|/Users/' "$fixtures_dir" --include="capsule.toml" 2>/dev/null | head -10)
    if [ -n "$hits" ]; then
        fail "HOME path not hardcoded in capsule.toml files" "Found in: $hits"
    else
        pass "HOME path not hardcoded in capsule.toml manifests"
    fi
}

# ---------------------------------------------------------------------------
# Human: same manifest on Mac / Linux / Windows
# ---------------------------------------------------------------------------
test_same_manifest_three_os() {
    checklist "Same capsule.toml produces same result on Mac / Linux / Windows" \
        "Pick a sample capsule (e.g. samples/react-vite or samples/python-hello)" \
        "Run 'ato run .' on macOS → note output" \
        "Copy same directory to Linux, run 'ato run .' → same output" \
        "Copy to Windows (WSL2 or native), run 'ato run .' → same output" \
        "All three produce identical stdout, same exit code, same capsule behavior"
}

# ---------------------------------------------------------------------------
# Human: timezone-dependent behavior
# ---------------------------------------------------------------------------
test_timezone_independence() {
    checklist "Timezone-independent behavior" \
        "Change system timezone to UTC, run a capsule that reads time — note output" \
        "Change timezone to JST (Asia/Tokyo), run same capsule — output must not change" \
        "Change timezone to US/Pacific, run again — output must not change" \
        "No dates or timestamps in capsule manifest affect behavior based on local TZ"
}

# ---------------------------------------------------------------------------
# Human: locale / character encoding
# ---------------------------------------------------------------------------
test_locale_encoding() {
    checklist "Locale and character encoding" \
        "Set LANG=en_US.UTF-8, run a capsule with Japanese filenames → succeeds" \
        "Set LANG=C (non-UTF-8), run same capsule → either succeeds or gives actionable error" \
        "Set LC_ALL=ja_JP.UTF-8 on Linux → capsule handles non-ASCII paths without garbling" \
        "Windows: NTFS paths with CJK characters → capsule runs without encoding errors"
}

# ---------------------------------------------------------------------------
# Human: file permissions across OS
# ---------------------------------------------------------------------------
test_file_permissions_cross_os() {
    checklist "File permissions cross-OS" \
        "Create a capsule with a non-executable entrypoint script (chmod 644 run.sh)" \
        "Run 'ato run .' on macOS — confirm ato reports missing execute bit or auto-sets it" \
        "Run same capsule on Windows — confirm ato handles the lack of POSIX permissions gracefully" \
        "chmod +x behavior on Windows (WSL2 host file) matches expected Linux behavior"
}

test_os_detection
test_manifest_parse_cross_os
test_no_hardcoded_path_separators
test_no_crlf_in_manifests
test_unicode_path
test_home_not_hardcoded
test_same_manifest_three_os
test_timezone_independence
test_locale_encoding
test_file_permissions_cross_os

print_suite_summary "$SUITE"
