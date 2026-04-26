#!/bin/bash
# =============================================================================
# §11 ato-api (ato.run) の実運用テスト
# =============================================================================
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../config.sh"
RESULT_FILE="$RESULTS_DIR/result_11_ato_store.log"
: > "$RESULT_FILE"

SUITE="§11 ato-api Operations"
echo "══════════════════════════════════"
echo " $SUITE"
echo "══════════════════════════════════"
check_ato

STORE_API_STAGING="https://staging.api.ato.run"
STORE_API_PROD="https://api.ato.run"
STORE_WEB_STAGING="https://staging.ato.run"
STORE_WEB_PROD="https://ato.run"

# ---------------------------------------------------------------------------
# Automated: store API health
# Staging failures are SKIP (staging infra may not be deployed);
# production failures are FAIL.
# ---------------------------------------------------------------------------
test_store_api_reachable() {
    for base in "$STORE_API_STAGING" "$STORE_API_PROD"; do
        local is_staging=false
        [[ "$base" == *staging* ]] && is_staging=true
        local status
        status=$(curl -sIo /dev/null -w "%{http_code}" --max-time 10 "$base/v1/capsules?limit=1" 2>/dev/null)
        case "$status" in
            200|304) pass "Store API reachable: $base (HTTP $status)" ;;
            401|403) pass "Store API reachable: $base (HTTP $status — auth required, endpoint exists)" ;;
            000)
                if $is_staging; then
                    skip "Store API staging unreachable: $base — staging infra not deployed (not a release blocker)"
                else
                    fail "Store API reachable" "$base — connection refused or network error"
                fi ;;
            *)
                if $is_staging; then
                    skip "Store API staging: $base — HTTP $status (staging may not be deployed)"
                else
                    fail "Store API reachable" "$base — HTTP $status"
                fi ;;
        esac
    done
}

# ---------------------------------------------------------------------------
# Automated: store web reachable
# Staging failures are SKIP; production failures are FAIL.
# ---------------------------------------------------------------------------
test_store_web_reachable() {
    for base in "$STORE_WEB_STAGING" "$STORE_WEB_PROD"; do
        local is_staging=false
        [[ "$base" == *staging* ]] && is_staging=true
        local status
        status=$(curl -sIo /dev/null -w "%{http_code}" --max-time 10 "$base" 2>/dev/null)
        case "$status" in
            200|304|301|302) pass "Store web reachable: $base (HTTP $status)" ;;
            000)
                if $is_staging; then
                    skip "Store web staging unreachable: $base — staging infra not deployed (not a release blocker)"
                else
                    fail "Store web reachable" "$base — connection refused"
                fi ;;
            *)
                if $is_staging; then
                    skip "Store web staging: $base — HTTP $status (staging may not be deployed)"
                else
                    fail "Store web reachable" "$base — HTTP $status"
                fi ;;
        esac
    done
}

# ---------------------------------------------------------------------------
# Automated: GET /v1/capsules returns valid JSON
# ---------------------------------------------------------------------------
test_store_list_endpoint() {
    local out="$ATO_TEST_TMP/store_list.txt"
    if curl -sf --max-time 15 "$STORE_API_PROD/v1/capsules?limit=1" -o "$out" 2>/dev/null; then
        if command -v jq &>/dev/null; then
            if jq -e '.' "$out" &>/dev/null; then
                pass "Store /v1/capsules returns valid JSON"
            else
                fail "Store /v1/capsules valid JSON" "Response is not valid JSON: $(head -c 200 "$out")"
            fi
        else
            if grep -q '{' "$out"; then
                pass "Store /v1/capsules returns JSON-shaped response (jq not available for full check)"
            else
                fail "Store /v1/capsules valid JSON" "Non-JSON response: $(head -c 200 "$out")"
            fi
        fi
    else
        fail "Store /v1/capsules endpoint" "curl failed — API may be down or require auth"
    fi
}

# ---------------------------------------------------------------------------
# Automated: publish a small capsule (requires auth)
# ---------------------------------------------------------------------------
test_publish_small_capsule() {
    local tmp_dir="$ATO_TEST_TMP/store-publish-test"
    mkdir -p "$tmp_dir"
    cat > "$tmp_dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "store-publish-test"
version = "0.1.0"
type = "app"
run = "python3 -c 'print(\"published\")'"
runtime = "source/python"
EOF
    provision_python_capsule "$tmp_dir"
    local out="$ATO_TEST_TMP/store_publish.txt"
    if ( cd "$tmp_dir" && run_cmd 60 "$out" ato publish ); then
        if grep -qiE "ato\.run/|published|success|upload" "$out"; then
            pass "ato publish — small capsule uploaded successfully"
        else
            pass "ato publish — exited 0 (check output for URL)"
        fi
    else
        if grep -qi "auth\|login\|unauthorized" "$out"; then
            skip "ato publish — requires authentication (run: ato login)"
        else
            fail "ato publish" "$(tail -5 "$out")"
        fi
    fi
    rm -rf "$tmp_dir"
}

# ---------------------------------------------------------------------------
# Automated: rate limit returns 429 on excess requests (structural check)
# ---------------------------------------------------------------------------
test_rate_limit_response_code() {
    # Flood the public list endpoint with requests and look for 429
    local got_429=false
    for i in $(seq 1 5); do
        local status
        status=$(curl -sIo /dev/null -w "%{http_code}" --max-time 5 \
            "$STORE_API_PROD/v1/capsules?limit=1" 2>/dev/null)
        [ "$status" = "429" ] && got_429=true && break
    done
    if $got_429; then
        pass "Rate limit enforced — received HTTP 429 under rapid requests"
    else
        skip "Rate limit — 5 requests did not trigger 429 (limit may be higher; use a script for real load test)"
    fi
}

# ---------------------------------------------------------------------------
# Human: concurrent publish
# ---------------------------------------------------------------------------
test_concurrent_publish() {
    checklist "Concurrent publish from same publisher" \
        "Run two 'ato publish' commands in parallel from the same account (different terminals)" \
        "Both complete successfully OR one gets a clear conflict error" \
        "No duplicate capsule entries or corrupt metadata in the store" \
        "Both share URLs are independently usable after the publishes complete"
}

# ---------------------------------------------------------------------------
# Human: large artifact upload
# ---------------------------------------------------------------------------
test_large_artifact_upload() {
    checklist "Large capsule publish (>500MB artifact)" \
        "Create a capsule that bundles a large model weight file (>500MB)" \
        "Run: ato publish <capsule-dir>" \
        "Upload progress is shown (not a silent hang)" \
        "Upload resumes if connection drops mid-transfer" \
        "Published capsule is downloadable via share URL by another user"
}

# ---------------------------------------------------------------------------
# Human: CDN propagation after publish
# ---------------------------------------------------------------------------
test_cdn_propagation() {
    checklist "CDN propagation — publish then pull from different regions" \
        "Publish a capsule from Region A (e.g. Japan)" \
        "Within 60 seconds, run: ato run <share-url> from Region B (e.g. US East)" \
        "The run succeeds (CDN cache has propagated)" \
        "Download speed from Region B is fast (CDN edge serving, not origin)"
}

# ---------------------------------------------------------------------------
# Human: publisher deletion behavior
# ---------------------------------------------------------------------------
test_publisher_deletion() {
    checklist "Publisher account deletion — capsule URL persistence" \
        "Note the share URL of a capsule from publisher X" \
        "Delete publisher X's account" \
        "Navigate to the share URL — does it 404 or show a 'publisher deleted' page?" \
        "Running 'ato run <share-url>' gives a clear 'capsule unavailable' error, not a panic" \
        "Document the behavior (permanent URL vs. linked-to-publisher)"
}

# ---------------------------------------------------------------------------
# Human: R2 / Cloudflare degradation
# ---------------------------------------------------------------------------
test_cloudflare_degradation() {
    checklist "Cloudflare / R2 degradation handling" \
        "Simulate an R2 outage (or wait for a real incident)" \
        "Run: ato publish — confirm graceful failure with retry hint" \
        "Run: ato run <capsule> — confirm graceful failure, not a silent hang" \
        "Check Cloudflare status page for incident correlation" \
        "After recovery, publish and run succeed without user action"
}

test_store_api_reachable
test_store_web_reachable
test_store_list_endpoint
test_publish_small_capsule
test_rate_limit_response_code
test_concurrent_publish
test_large_artifact_upload
test_cdn_propagation
test_publisher_deletion
test_cloudflare_degradation

print_suite_summary "$SUITE"
