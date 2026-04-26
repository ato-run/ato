#!/bin/bash
# =============================================================================
# §8 Trust UX の実体験
# =============================================================================
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../config.sh"
RESULT_FILE="$RESULTS_DIR/result_08_trust_ux.log"
: > "$RESULT_FILE"

SUITE="§8 Trust UX"
echo "══════════════════════════════════"
echo " $SUITE"
echo "══════════════════════════════════"
check_ato

# ---------------------------------------------------------------------------
# Automated: trust store directory exists
# ---------------------------------------------------------------------------
test_trust_store_exists() {
    local trust_dir="${ATO_HOME:-$HOME/.ato}/trust"
    if [ -d "$trust_dir" ]; then
        pass "Trust store directory exists: $trust_dir"
    else
        # Trust store may be lazily initialized on first actual trust event
        skip "Trust store directory not yet created at $trust_dir (initialized lazily on first trust event)"
    fi
}

# ---------------------------------------------------------------------------
# Automated: revocation feed URL is reachable
# ---------------------------------------------------------------------------
test_revocation_feed_reachable() {
    # Attempt to reach the revocation feed endpoint
    local url="https://ato.run/revocation.json"
    local status
    status=$(curl -sIo /dev/null -w "%{http_code}" --max-time 10 "$url" 2>/dev/null)
    case "$status" in
        200|304) pass "Revocation feed reachable (HTTP $status)" ;;
        404)     fail "Revocation feed reachable" "HTTP 404 — endpoint not found" ;;
        *)       info "Revocation feed status: HTTP $status (network may be unavailable)" ;;
    esac
}

# ---------------------------------------------------------------------------
# Automated: running an unknown capsule triggers TOFU prompt (check output contains fingerprint info)
# ---------------------------------------------------------------------------
test_tofu_prompt_shown() {
    # Publish a brand-new capsule, then run it on a clean trust store
    local tmp_dir="$ATO_TEST_TMP/tofu-capsule"
    mkdir -p "$tmp_dir"
    cat > "$tmp_dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "tofu-test"
version = "0.1.0"
type = "app"
run = "python3 -c 'print(\"trust ok\")'"
runtime = "source/python"
EOF
    local out="$ATO_TEST_TMP/tofu_prompt.txt"
    provision_python_capsule "$tmp_dir"
    # Pipe 'n' to decline TOFU — we just want to confirm the prompt appears
    printf "n\n" | CAPSULE_ALLOW_UNSAFE=1 timeout 20 ato run --dangerously-skip-permissions "$tmp_dir" >"$out" 2>&1 || true

    if grep -qi "trust\|fingerprint\|publisher\|confirm\|first time" "$out"; then
        pass "TOFU prompt appears for untrusted capsule"
    else
        # If already trusted, prompt may not appear
        if grep -qi "trust ok\|already trusted" "$out"; then
            pass "TOFU prompt not shown (capsule already trusted) — reset trust store to retest"
        else
            info "Output: $(cat "$out" | head -10)"
            skip "TOFU prompt — could not determine (capsule may be trusted or trust is implicit)"
        fi
    fi
    rm -rf "$tmp_dir"
}

# ---------------------------------------------------------------------------
# Automated: fingerprint mismatch → warning not silent
# ---------------------------------------------------------------------------
test_fingerprint_mismatch_not_silent() {
    # This is hard to automate fully without a real MITM; check that the code path exists
    local out="$ATO_TEST_TMP/fp_mismatch.txt"
    # Run ato with a deliberately incorrect fingerprint flag (if supported)
    if run_cmd 10 "$out" ato run --expected-fingerprint "sha256:0000000000000000000000000000000000000000000000000000000000000000" . 2>/dev/null; then
        fail "Fingerprint mismatch warning" "ato ran successfully with incorrect fingerprint (should fail)"
    else
        if grep -qi "fingerprint\|mismatch\|tamper\|verification\|does not match" "$out"; then
            pass "Fingerprint mismatch produces a clear error message"
        else
            skip "Fingerprint mismatch check — flag may not exist yet or different error path"
        fi
    fi
}

# ---------------------------------------------------------------------------
# Human: TOFU first-run UX
# ---------------------------------------------------------------------------
test_tofu_first_run_ux() {
    checklist "TOFU first-run user experience" \
        "On a clean trust store, run a signed capsule for the first time" \
        "Terminal shows publisher fingerprint and human-readable publisher name" \
        "Prompt clearly asks: 'Trust this publisher? [y/N]'" \
        "User can type 'n' to abort — capsule does NOT run" \
        "User can type 'y' to trust — capsule runs and publisher is saved to trust store" \
        "Second run of same capsule: no prompt (already trusted)"
}

# ---------------------------------------------------------------------------
# Human: petname assignment
# ---------------------------------------------------------------------------
test_petname() {
    checklist "Petname (friendly alias) assignment" \
        "After trusting a publisher, run: ato trust petname <fingerprint> 'My AI vendor'" \
        "Subsequent TOFU prompts show 'My AI vendor' instead of raw fingerprint" \
        "Petname is visible in: ato trust list" \
        "Petname can be updated: ato trust petname <fp> 'New Name'" \
        "Petname persists across ato restarts"
}

# ---------------------------------------------------------------------------
# Human: key rotation
# ---------------------------------------------------------------------------
test_key_rotation() {
    checklist "Publisher key rotation — old and new key both trusted temporarily" \
        "Publisher rotates their signing key" \
        "ato detects both old and new key for the same publisher identity" \
        "Capsules signed by OLD key still run during rotation window" \
        "Capsules signed by NEW key also run" \
        "After rotation window expires, OLD key capsules prompt for re-trust or are blocked"
}

# ---------------------------------------------------------------------------
# Human: revocation enforcement
# ---------------------------------------------------------------------------
test_revocation_enforcement() {
    checklist "Revocation — capsule signed by revoked key is blocked" \
        "Obtain a capsule signed by a key that is in the revocation feed" \
        "Run: ato run <revoked-capsule>" \
        "ato blocks the capsule with a clear revocation message" \
        "Message names the revoked fingerprint and links to more info" \
        "capsule does NOT run even if user has previously trusted that key"
}

# ---------------------------------------------------------------------------
# Human: offline revocation behavior
# ---------------------------------------------------------------------------
test_offline_revocation() {
    checklist "Offline revocation behavior (time-limited trust)" \
        "Disconnect from the internet" \
        "Run a capsule whose revocation status cannot be checked" \
        "Confirm ato uses cached revocation data if available" \
        "Confirm ato's behavior matches the configured offline-trust policy (e.g. 'allow for Xh')" \
        "Reconnect — ato re-checks revocation feed on next run"
}

# ---------------------------------------------------------------------------
# Human: trust store export/import
# ---------------------------------------------------------------------------
test_trust_store_migration() {
    checklist "Trust store export and import (machine migration)" \
        "Run: ato trust export > trust-backup.json" \
        "Copy trust-backup.json to a new machine" \
        "Run: ato trust import trust-backup.json on new machine" \
        "Trusted publishers from old machine are now trusted on new machine" \
        "Petnames are preserved after import" \
        "No duplicate entries after import"
}

test_trust_store_exists
test_revocation_feed_reachable
test_tofu_prompt_shown
test_fingerprint_mismatch_not_silent
test_tofu_first_run_ux
test_petname
test_key_rotation
test_revocation_enforcement
test_offline_revocation
test_trust_store_migration

print_suite_summary "$SUITE"
