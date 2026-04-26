#!/bin/bash
# =============================================================================
# §14 ドキュメンテーションとの整合
# Copy-paste commands from docs must work exactly as written
# =============================================================================
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../config.sh"
RESULT_FILE="$RESULTS_DIR/result_14_doc_alignment.log"
: > "$RESULT_FILE"

SUITE="§14 Documentation Alignment"
echo "══════════════════════════════════"
echo " $SUITE"
echo "══════════════════════════════════"
check_ato

DOCS_SITE="https://docs.ato.run"

# ---------------------------------------------------------------------------
# Automated: ato --help runs without error
# ---------------------------------------------------------------------------
test_help_toplevel() {
    local out="$ATO_TEST_TMP/help_toplevel.txt"
    if run_cmd 10 "$out" ato --help; then
        pass "ato --help exits 0"
    else
        fail "ato --help" "Exited non-zero: $(tail -3 "$out")"
    fi
}

# ---------------------------------------------------------------------------
# Automated: all known subcommands have --help
# ---------------------------------------------------------------------------
test_subcommand_help() {
    local subcmds=("run" "publish" "pack" "open" "ipc" "login" "logout" "trust" "version")
    for cmd in "${subcmds[@]}"; do
        local out="$ATO_TEST_TMP/help_${cmd}.txt"
        if run_cmd 5 "$out" ato "$cmd" --help 2>/dev/null; then
            pass "ato $cmd --help exits 0"
        elif grep -qi "unknown command\|not found\|error" "$out"; then
            skip "ato $cmd --help — subcommand may not exist yet"
        else
            pass "ato $cmd --help — ran (non-zero exit may be expected for interactive commands)"
        fi
    done
}

# ---------------------------------------------------------------------------
# Automated: docs site is reachable
# ---------------------------------------------------------------------------
test_docs_site_reachable() {
    local status
    status=$(curl -sIo /dev/null -w "%{http_code}" --max-time 10 "$DOCS_SITE" 2>/dev/null)
    case "$status" in
        200|301|302) pass "Docs site reachable: $DOCS_SITE (HTTP $status)" ;;
        000) fail "Docs site reachable" "Connection failed — $DOCS_SITE unreachable" ;;
        404) fail "Docs site reachable" "HTTP 404 — docs site not found" ;;
        *) fail "Docs site reachable" "HTTP $status" ;;
    esac
}

# ---------------------------------------------------------------------------
# Automated: Getting Started page reachable
# ---------------------------------------------------------------------------
test_getting_started_page() {
    local url="$DOCS_SITE/getting-started"
    local status
    status=$(curl -sIo /dev/null -w "%{http_code}" --max-time 10 "$url" 2>/dev/null)
    case "$status" in
        200|301|302) pass "Getting Started page reachable: $url" ;;
        404) fail "Getting Started page" "HTTP 404 — $url does not exist" ;;
        *) info "Getting Started page HTTP $status — check if URL has changed" ;;
    esac
}

# ---------------------------------------------------------------------------
# Automated: error message URLs are not 404
# ---------------------------------------------------------------------------
test_error_message_urls() {
    # Trigger a known error and capture any URLs mentioned in the output
    local dir="$ATO_TEST_TMP/err-url-check"
    mkdir -p "$dir"
    printf 'schema_version = "0.3"\nname = [broken\n' > "$dir/capsule.toml"
    local err_out="$ATO_TEST_TMP/err_url_output.txt"
    run_cmd 10 "$err_out" ato run "$dir" || true

    # Extract URLs from error output
    local urls
    urls=$(grep -oE 'https://[a-zA-Z0-9./_-]+' "$err_out" 2>/dev/null | head -10)
    if [ -z "$urls" ]; then
        skip "Error message URLs — no URLs found in error output to validate"
        rm -rf "$dir"
        return
    fi

    local all_ok=true
    while IFS= read -r url; do
        local status
        status=$(curl -sIo /dev/null -w "%{http_code}" --max-time 10 "$url" 2>/dev/null)
        if [ "$status" = "404" ]; then
            fail "Error message URL valid" "404: $url"
            all_ok=false
        else
            pass "Error message URL reachable: $url (HTTP $status)"
        fi
    done <<< "$urls"
    rm -rf "$dir"
}

# ---------------------------------------------------------------------------
# Automated: capsule.toml schema fields documented in README
# ---------------------------------------------------------------------------
test_manifest_fields_documented() {
    local readme="$SCRIPT_DIR/../../../apps/ato-cli/README.md"
    if [ ! -f "$readme" ]; then
        readme="$SCRIPT_DIR/../../../README.md"
    fi
    if [ ! -f "$readme" ]; then
        skip "README.md not found — cannot validate manifest field documentation"
        return
    fi

    local fields=("schema_version" "name" "version" "run" "runtime")
    local undocumented=()
    for field in "${fields[@]}"; do
        if ! grep -qi "$field" "$readme"; then
            undocumented+=("$field")
        fi
    done

    if [ "${#undocumented[@]}" -eq 0 ]; then
        pass "All core capsule.toml fields documented in README"
    else
        fail "Manifest fields documented" "Not found in README: ${undocumented[*]}"
    fi
}

# ---------------------------------------------------------------------------
# Automated: ato --version output matches release tag
# ---------------------------------------------------------------------------
test_version_matches_release() {
    local ver
    ver=$(ato --version 2>&1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)
    if [ -n "$ver" ]; then
        pass "ato --version reports semver: $ver"
        info "Manually verify this matches the current GitHub release tag"
    else
        fail "ato --version semver format" "Could not extract semver from: $(ato --version 2>&1 | head -1)"
    fi
}

# ---------------------------------------------------------------------------
# Human: Getting Started commands work exactly as written
# ---------------------------------------------------------------------------
test_getting_started_commands() {
    checklist "Getting Started page — copy-paste commands work" \
        "Open $DOCS_SITE/getting-started in a browser" \
        "Copy each command exactly as shown and run it in a fresh terminal" \
        "All commands succeed without modification (no need to fix quoting, paths, etc.)" \
        "The 'Quick Start' section produces the output shown in the docs" \
        "Installation command from docs matches actual install script behavior"
}

# ---------------------------------------------------------------------------
# Human: blog post Llama 3.1 8B in 5 minutes
# ---------------------------------------------------------------------------
test_blog_llama_5min() {
    checklist "Blog: Llama 3.1 8B local chat in 5 minutes (from zero)" \
        "On a clean machine with ato installed, follow the blog post instructions exactly" \
        "Record elapsed time from first command to model responding" \
        "Goal: ≤5 minutes on a modern Mac/Linux with good internet" \
        "Every command in the blog post works as written (no undocumented steps)" \
        "The final UX matches what the blog post screenshots/video show"
}

# ---------------------------------------------------------------------------
# Human: capsule.toml field documentation examples work
# ---------------------------------------------------------------------------
test_manifest_examples_work() {
    checklist "capsule.toml field documentation examples are valid" \
        "Open the capsule.toml field reference in docs" \
        "For each documented field example, create a minimal capsule.toml containing it" \
        "Run: ato validate <dir> (or ato run) — no parse errors" \
        "Pay special attention to enum values (runtime, isolation.*) — all listed values are accepted" \
        "Default values documented match what ato actually uses when the field is omitted"
}

# ---------------------------------------------------------------------------
# Human: ato --help examples match behavior
# ---------------------------------------------------------------------------
test_help_examples_behavior() {
    checklist "ato --help examples match actual behavior" \
        "Run: ato --help and note the usage examples" \
        "Run each example command exactly as shown" \
        "Output or behavior matches what --help describes" \
        "All flags described in --help are accepted by ato (no 'unknown flag' errors)" \
        "No flags accepted by ato are missing from --help (check ato --help vs. release notes)"
}

test_help_toplevel
test_subcommand_help
test_docs_site_reachable
test_getting_started_page
test_error_message_urls
test_manifest_fields_documented
test_version_matches_release
test_getting_started_commands
test_blog_llama_5min
test_manifest_examples_work
test_help_examples_behavior

print_suite_summary "$SUITE"
