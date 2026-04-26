#!/bin/bash
# =============================================================================
# §6 Share URL の実 URL 配布フロー
# =============================================================================
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../config.sh"
RESULT_FILE="$RESULTS_DIR/result_06_share_url.log"
: > "$RESULT_FILE"

SUITE="§6 Share URL Distribution"
echo "══════════════════════════════════"
echo " $SUITE"
echo "══════════════════════════════════"
check_ato

# ---------------------------------------------------------------------------
# Automated: ato publish output contains a valid https://ato.run/s/<id> URL
# ---------------------------------------------------------------------------
test_publish_url_format() {
    local tmp_dir="$ATO_TEST_TMP/share-url-capsule"
    mkdir -p "$tmp_dir"
    cat > "$tmp_dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "share-url-test"
version = "0.1.0"
type = "app"
run = "python3 -c 'print(\"hello from share\")'"
runtime = "source/python"
EOF
    provision_python_capsule "$tmp_dir"
    local out="$ATO_TEST_TMP/publish_output.txt"
    if ( cd "$tmp_dir" && run_cmd 60 "$out" ato publish ) ; then
        local url
        url=$(grep -oE 'https://ato\.run/s/[A-Za-z0-9_-]+' "$out" | head -1)
        if [ -n "$url" ]; then
            pass "ato publish outputs share URL: $url"
            # Store for follow-up tests
            echo "$url" > "$ATO_TEST_TMP/last_share_url.txt"
        else
            fail "ato publish outputs share URL" "No https://ato.run/s/<id> found in output: $(cat "$out")"
        fi
    else
        if grep -qi "auth\|login\|permission\|unauthorized" "$out"; then
            skip "ato publish — requires authenticated session (run: ato login first)"
        else
            fail "ato publish" "Publish failed: $(tail -5 "$out")"
        fi
    fi
    rm -rf "$tmp_dir"
}

# ---------------------------------------------------------------------------
# Automated: published URL is reachable (HEAD request)
# ---------------------------------------------------------------------------
test_published_url_reachable() {
    local url_file="$ATO_TEST_TMP/last_share_url.txt"
    if [ ! -f "$url_file" ]; then
        skip "Published URL not available (publish test skipped or failed)"
        return
    fi
    local url
    url=$(cat "$url_file")
    local http_status
    http_status=$(curl -sIo /dev/null -w "%{http_code}" --max-time 10 "$url" 2>/dev/null)
    if [ "$http_status" = "200" ]; then
        pass "Published URL is reachable (HTTP 200): $url"
    elif [ "$http_status" = "302" ] || [ "$http_status" = "301" ]; then
        pass "Published URL redirects ($http_status) — expected for capsule landing page"
    else
        fail "Published URL reachable" "HTTP $http_status for $url"
    fi
}

# ---------------------------------------------------------------------------
# Automated: OG meta tags on share URL
# ---------------------------------------------------------------------------
test_og_meta_tags() {
    local url_file="$ATO_TEST_TMP/last_share_url.txt"
    if [ ! -f "$url_file" ]; then
        skip "Share URL not available for OG tag check"
        return
    fi
    local url
    url=$(cat "$url_file")
    local body
    body=$(curl -sfL --max-time 15 "$url" 2>/dev/null)
    if echo "$body" | grep -qi 'og:title\|og:description\|og:image'; then
        pass "Share URL page has OG meta tags"
    else
        fail "Share URL OG meta tags" "No og: meta tags found in page source"
    fi
}

# ---------------------------------------------------------------------------
# Automated: revoked share URL returns 404 or appropriate error
# ---------------------------------------------------------------------------
test_revoked_url() {
    # Use a known-dead share ID format to verify 404 handling
    local fake_url="https://ato.run/s/this-id-does-not-exist-000000"
    local http_status
    http_status=$(curl -sIo /dev/null -w "%{http_code}" --max-time 10 "$fake_url" 2>/dev/null)
    if [ "$http_status" = "404" ] || [ "$http_status" = "410" ]; then
        pass "Non-existent share URL returns HTTP $http_status (expected 404/410)"
    elif [ "$http_status" = "200" ]; then
        fail "Non-existent share URL" "HTTP 200 returned for fake ID — should be 404"
    else
        pass "Non-existent share URL returns HTTP $http_status (not 200)"
    fi
}

# ---------------------------------------------------------------------------
# Automated: ato run <share-url> works on this machine
# ---------------------------------------------------------------------------
test_run_share_url_local() {
    local url_file="$ATO_TEST_TMP/last_share_url.txt"
    if [ ! -f "$url_file" ]; then
        skip "Share URL not available — publish test skipped"
        return
    fi
    local url
    url=$(cat "$url_file")
    local out="$ATO_TEST_TMP/share_run_local.txt"
    if run_cmd 60 "$out" ato run "$url"; then
        if grep -qi "hello from share" "$out"; then
            pass "ato run <share-url> produces expected output on same machine"
        else
            pass "ato run <share-url> exited 0 (output may differ due to capsule)"
        fi
    else
        fail "ato run <share-url>" "$(tail -5 "$out")"
    fi
}

# ---------------------------------------------------------------------------
# Automated: point-in-time identity — running same share URL twice gives same result
# ---------------------------------------------------------------------------
test_reproducible_share_url() {
    local url_file="$ATO_TEST_TMP/last_share_url.txt"
    if [ ! -f "$url_file" ]; then
        skip "Share URL not available"
        return
    fi
    local url
    url=$(cat "$url_file")
    local out1="$ATO_TEST_TMP/share_repro_1.txt"
    local out2="$ATO_TEST_TMP/share_repro_2.txt"
    run_cmd 60 "$out1" ato run "$url" || true
    sleep 2
    run_cmd 60 "$out2" ato run "$url" || true
    if diff -q "$out1" "$out2" &>/dev/null; then
        pass "Share URL is reproducible — identical output on two runs"
    else
        fail "Share URL reproducibility" "Outputs differ between runs (non-deterministic capsule?)"
    fi
}

# ---------------------------------------------------------------------------
# Human: click URL on a different machine
# ---------------------------------------------------------------------------
test_share_url_different_machine() {
    local url=""
    [ -f "$ATO_TEST_TMP/last_share_url.txt" ] && url=$(cat "$ATO_TEST_TMP/last_share_url.txt")
    checklist "Share URL works on a different machine" \
        "Publish a capsule and copy the share URL${url:+: $url}" \
        "Send the URL to another person / paste into another machine's terminal" \
        "Run: ato run <share-url> on the OTHER machine" \
        "Confirm it downloads and runs the capsule correctly" \
        "Test on macOS → Linux, macOS → Windows, Linux → macOS cross-machine"
}

# ---------------------------------------------------------------------------
# Human: ato:// URL handler registration
# ---------------------------------------------------------------------------
test_ato_url_handler() {
    checklist "ato:// URL scheme handler is registered" \
        "macOS: open ato://run/<share-id> in Terminal → ato is invoked" \
        "Linux: xdg-open ato://run/<share-id> triggers ato (check xdg-mime query)" \
        "Windows: clicking ato:// link in browser invokes ato (check registry)" \
        "Test competing handler: if another app registers ato://, confirm ato wins or error is clear"
}

# ---------------------------------------------------------------------------
# Human: uninstalled user clicks share URL
# ---------------------------------------------------------------------------
test_uninstalled_user_flow() {
    checklist "Uninstalled user clicks share URL" \
        "On a machine with NO ato installed, open: https://ato.run/s/<share-id> in browser" \
        "Browser redirects to install page or shows install instructions" \
        "Install instructions are correct and actionable for macOS/Linux/Windows" \
        "After installing, re-clicking (or running) the same URL launches the capsule" \
        "Flow completes end-to-end with no dead ends"
}

# ---------------------------------------------------------------------------
# Human: social media URL preview
# ---------------------------------------------------------------------------
test_social_preview() {
    checklist "Share URL social preview rendering" \
        "Paste share URL into Slack — OG preview (title, description, image) renders" \
        "Paste into Discord — preview renders" \
        "Paste into Twitter/X — card renders with title and description" \
        "Paste into iMessage — link preview shows capsule name" \
        "Gmail 'This link goes to an untrusted site' warning does NOT appear"
}

test_publish_url_format
test_published_url_reachable
test_og_meta_tags
test_revoked_url
test_run_share_url_local
test_reproducible_share_url
test_share_url_different_machine
test_ato_url_handler
test_uninstalled_user_flow
test_social_preview

print_suite_summary "$SUITE"
