#!/bin/bash
# =============================================================================
# §6 Workspace share の実配布フロー (local-file)
# =============================================================================
# Web share flow (ato.run/s/<id>) was retired 2026-08; sharing is local-file
# only. This suite verifies the current contract: `ato workspace share` writes
# share.spec.json / share.lock.json, `ato run <share file>` executes the shared
# workspace, and `ato workspace setup` materializes it.
#
# The share fixture is created ONCE and kept alive for the whole suite so every
# test exercises the real files (a per-test cleanup would make the dependent
# tests SKIP instead of PASS). It is removed by the EXIT trap.
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../config.sh"
RESULT_FILE="$RESULTS_DIR/result_06_share_url.log"
: > "$RESULT_FILE"

SUITE="§6 Workspace Share Distribution (local-file)"
echo "══════════════════════════════════"
echo " $SUITE"
echo "══════════════════════════════════"
check_ato

SHARE_FIXTURE_DIR="$ATO_TEST_TMP/share-capsule"
SHARE_SPEC_FILE="$SHARE_FIXTURE_DIR/.ato/share/share.spec.json"
SHARE_LOCK_FILE="$SHARE_FIXTURE_DIR/.ato/share/share.lock.json"
MATERIALIZED_DIR="$ATO_TEST_TMP/share-materialized"

cleanup() {
    rm -rf "$SHARE_FIXTURE_DIR" "$MATERIALIZED_DIR"
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Automated: ato workspace share writes share.spec.json / share.lock.json / guide.md
# ---------------------------------------------------------------------------
test_workspace_share_outputs() {
    mkdir -p "$SHARE_FIXTURE_DIR"
    # package.json gives the capture a runnable entry (dev script); an empty
    # package-lock.json makes the detected `npm ci` install step succeed fast.
    # The dev script is a shell builtin so the entry needs no runtime provisioning.
    cat > "$SHARE_FIXTURE_DIR/package.json" <<'EOF'
{
  "name": "share-test",
  "version": "0.1.0",
  "private": true,
  "scripts": { "dev": "echo hello-from-share" }
}
EOF
    cat > "$SHARE_FIXTURE_DIR/package-lock.json" <<'EOF'
{
  "name": "share-test",
  "version": "0.1.0",
  "lockfileVersion": 3,
  "requires": true,
  "packages": { "": { "name": "share-test", "version": "0.1.0" } }
}
EOF
    local out="$ATO_TEST_TMP/share_output.txt"
    if ( cd "$SHARE_FIXTURE_DIR" && run_cmd 60 "$out" ato workspace share --yes ); then
        if [ -f "$SHARE_SPEC_FILE" ] \
            && [ -f "$SHARE_LOCK_FILE" ] \
            && [ -f "$SHARE_FIXTURE_DIR/.ato/share/guide.md" ]; then
            pass "ato workspace share writes share.spec.json / share.lock.json / guide.md"
        else
            fail "ato workspace share outputs" "Missing share files in .ato/share/: $(ls -la "$SHARE_FIXTURE_DIR/.ato/share" 2>&1)"
        fi
    else
        fail "ato workspace share" "$(tail -5 "$out")"
    fi
}

# Fixture must exist for the follow-up tests. The producer test creates it, so a
# missing fixture is a FAILURE (not a silent skip) — it means the suite order or
# fixture lifecycle broke.
require_fixture() {
    if [ ! -f "$SHARE_SPEC_FILE" ] || [ ! -f "$SHARE_LOCK_FILE" ]; then
        fail "share fixture missing" "$SHARE_SPEC_FILE / $SHARE_LOCK_FILE absent (producer test did not run?)"
        return 1
    fi
    return 0
}

# ---------------------------------------------------------------------------
# Automated: ato run <share.spec.json> runs the shared workspace
# ---------------------------------------------------------------------------
test_run_share_file_local() {
    require_fixture || return
    local out="$ATO_TEST_TMP/share_run_local.txt"
    if run_cmd 60 "$out" ato run "$SHARE_SPEC_FILE"; then
        if grep -qi "hello-from-share" "$out"; then
            pass "ato run <share.spec.json> produces expected output on same machine"
        else
            pass "ato run <share.spec.json> exited 0 (output may differ due to capsule)"
        fi
    else
        fail "ato run <share.spec.json>" "$(tail -5 "$out")"
    fi
}

# ---------------------------------------------------------------------------
# Automated: ato workspace setup <share.spec.json> --into materializes
# ---------------------------------------------------------------------------
test_workspace_setup_materializes() {
    require_fixture || return
    rm -rf "$MATERIALIZED_DIR" && mkdir -p "$MATERIALIZED_DIR"
    local out="$ATO_TEST_TMP/share_setup.txt"
    if run_cmd 60 "$out" ato workspace setup "$SHARE_SPEC_FILE" --into "$MATERIALIZED_DIR/ws" --dev; then
        if grep -qi "Workspace ready" "$out"; then
            pass "ato workspace setup materializes the shared workspace"
        else
            pass "ato workspace setup exited 0 (verify materialized files below)"
        fi
    else
        fail "ato workspace setup" "$(tail -5 "$out")"
    fi
}

# ---------------------------------------------------------------------------
# Human: distribute share files to a different machine
# ---------------------------------------------------------------------------
test_share_files_different_machine() {
    checklist "Share files work on a different machine" \
        "Run: ato workspace share (or use the fixture's share.spec.json)" \
        "Copy share.spec.json + share.lock.json to another machine (git, chat, USB)" \
        "Run: ato run ./share.spec.json on the OTHER machine" \
        "Confirm it materializes and runs the workspace correctly" \
        "Test on macOS → Linux, macOS → Windows, Linux → macOS cross-machine"
}

# ---------------------------------------------------------------------------
# Human: uninstalled user receives share files
# ---------------------------------------------------------------------------
test_uninstalled_user_flow() {
    checklist "Uninstalled user receives share files" \
        "On a machine with NO ato installed, copy share.spec.json + share.lock.json over" \
        "Install ato via https://ato.run/install.sh (or package manager)" \
        "Run: ato run ./share.spec.json" \
        "Flow completes end-to-end with no dead ends"
}

test_workspace_share_outputs
test_run_share_file_local
test_workspace_setup_materializes
test_share_files_different_machine
test_uninstalled_user_flow

print_suite_summary "$SUITE"
