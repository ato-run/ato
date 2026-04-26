#!/bin/bash
# =============================================================================
# §1 インストール / アップグレード経路
# =============================================================================
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../config.sh"
RESULT_FILE="$RESULTS_DIR/result_01_install_upgrade.log"
: > "$RESULT_FILE"

SUITE="§1 Install / Upgrade"
echo "══════════════════════════════════"
echo " $SUITE"
echo "══════════════════════════════════"
check_ato

# ---------------------------------------------------------------------------
# Automated: install script URL is reachable
# ---------------------------------------------------------------------------
test_install_script_reachable() {
    local url="https://ato.run/install.sh"
    local out="$ATO_TEST_TMP/install_sh_head.txt"
    if curl -sfI "$url" -o "$out" --max-time 10 2>&1; then
        local status
        status=$(grep -i "^HTTP/" "$out" | tail -1 | awk '{print $2}')
        if [ "$status" = "200" ]; then
            pass "install.sh URL reachable (HTTP 200)"
        else
            fail "install.sh URL reachable" "HTTP $status"
        fi
    else
        fail "install.sh URL reachable" "curl failed — network error or URL does not exist"
    fi
}

# ---------------------------------------------------------------------------
# Automated: ato --version returns semver
# ---------------------------------------------------------------------------
test_version_format() {
    local ver
    ver=$(ato --version 2>&1 | head -1)
    if echo "$ver" | grep -qE '[0-9]+\.[0-9]+\.[0-9]+'; then
        pass "ato --version returns semver ($ver)"
    else
        fail "ato --version returns semver" "Got: $ver"
    fi
}

# ---------------------------------------------------------------------------
# Automated: ~/.ato/ structure exists after install
# ---------------------------------------------------------------------------
test_ato_home_structure() {
    local ato_home="${ATO_HOME:-$HOME/.ato}"
    if [ -d "$ato_home" ]; then
        pass "~/.ato/ directory exists"
    else
        fail "~/.ato/ directory exists" "Expected $ato_home to exist after install"
    fi
}

# ---------------------------------------------------------------------------
# Human: fresh install on clean machine
# ---------------------------------------------------------------------------
test_fresh_install() {
    checklist "Fresh install on clean machine (no ~/.ato/)" \
        "On a machine where ato has never been installed, run: curl ato.run/install.sh | sh" \
        "Verify PATH is updated in shell config (.bashrc / .zshrc / fish.config)" \
        "Open a new terminal and run: ato --version" \
        "Confirm no leftover files in /tmp or unexpected locations" \
        "Repeat on macOS, Linux, and Windows (WSL2)"
}

# ---------------------------------------------------------------------------
# Human: upgrade path
# ---------------------------------------------------------------------------
test_upgrade() {
    checklist "Upgrade from previous version" \
        "Install the previous release of ato" \
        "Note current ~/.ato/ layout (ls -la ~/.ato/)" \
        "Run the upgrade command (curl ato.run/install.sh | sh  OR  ato self-update)" \
        "Verify ato --version shows the new version" \
        "Verify ~/.ato/ migration ran (no broken dirs, capsule.lock.json present if applicable)" \
        "Verify old ato.lock.json capsules still work: ato run <old-capsule-dir>" \
        "Confirm ato.lock.json → capsule.lock.json migration completed if applicable"
}

# ---------------------------------------------------------------------------
# Human: uninstall
# ---------------------------------------------------------------------------
test_uninstall() {
    checklist "Uninstall — no residue" \
        "Run the uninstall procedure (ato uninstall or manual rm -rf ~/.ato/)" \
        "Confirm no launch agents / systemd units remain active" \
        "Confirm PATH entries for ato are removed from shell configs" \
        "Confirm /usr/local/bin/ato (or equivalent) is gone" \
        "Confirm ato --version returns 'not found'"
}

# ---------------------------------------------------------------------------
# Human: offline install
# ---------------------------------------------------------------------------
test_offline_install() {
    checklist "Offline install from tarball" \
        "Download the release tarball on a connected machine" \
        "Transfer to a machine with no internet (scp / USB)" \
        "Extract and run the install script offline" \
        "Verify ato --version returns the correct version"
}

# ---------------------------------------------------------------------------
# Human: multiple version coexistence
# ---------------------------------------------------------------------------
test_multi_version() {
    checklist "Multiple version coexistence" \
        "Install ato@0.5 to a custom prefix (e.g. ~/.ato-0.5/)" \
        "Install ato@latest to default prefix" \
        "Run both binaries and confirm each reports its own version" \
        "Confirm they do not corrupt each other's store"
}

test_install_script_reachable
test_version_format
test_ato_home_structure
test_fresh_install
test_upgrade
test_uninstall
test_offline_install
test_multi_version

print_suite_summary "$SUITE"
