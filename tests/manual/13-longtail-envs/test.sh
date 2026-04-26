#!/bin/bash
# =============================================================================
# §13 ロングテールの環境
# =============================================================================
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../config.sh"
RESULT_FILE="$RESULTS_DIR/result_13_longtail_envs.log"
: > "$RESULT_FILE"

SUITE="§13 Long-Tail Environments"
echo "══════════════════════════════════"
echo " $SUITE"
echo "══════════════════════════════════"
check_ato

# ---------------------------------------------------------------------------
# Automated: detect current environment and report
# ---------------------------------------------------------------------------
test_environment_fingerprint() {
    info "OS: $(uname -srm)"
    info "Kernel: $(uname -r)"
    info "HOME: $HOME"
    info "Shell: ${SHELL:-unknown}"

    # Detect NixOS
    if [ -f /etc/NIXOS ] || grep -qi "NixOS" /etc/os-release 2>/dev/null; then
        info "Detected: NixOS (FHS non-compliant filesystem)"
    fi
    # Detect Asahi Linux
    if grep -qi "asahi" /etc/os-release 2>/dev/null || \
       grep -qi "asahi" /proc/cpuinfo 2>/dev/null; then
        info "Detected: Asahi Linux (Apple Silicon)"
    fi
    # Detect ARM
    if uname -m | grep -qi "arm\|aarch64"; then
        info "Detected: ARM architecture ($(uname -m))"
    fi
    # Detect encrypted home
    if command -v fscrypt &>/dev/null || [ -d "$HOME/.fscrypt" ]; then
        info "Detected: Possible fscrypt/encrypted home"
    fi
    # Detect cloud sync of home
    for sync_dir in "$HOME/Library/Mobile Documents" "$HOME/OneDrive" "$HOME/Dropbox"; do
        [ -d "$sync_dir" ] && info "Detected: Cloud-synced home directory at $sync_dir"
    done

    pass "Environment fingerprint collected (see INFO lines above)"
}

# ---------------------------------------------------------------------------
# Automated: basic capsule run on current architecture
# ---------------------------------------------------------------------------
test_basic_run_this_platform() {
    local dir="$ATO_TEST_TMP/longtail-basic"
    mkdir -p "$dir"
    cat > "$dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "longtail-basic"
version = "0.1.0"
type = "app"
run = "python3 -c 'import platform; print(platform.machine(), platform.system())'"
runtime = "source/python"
EOF
    local out="$ATO_TEST_TMP/longtail_basic.txt"
    provision_python_capsule "$dir"
    # ato run hangs after capsule exits (known bug), check output content instead
    run_cmd 30 "$out" env CAPSULE_ALLOW_UNSAFE=1 ato run --dangerously-skip-permissions "$dir" || true
    local capsule_out
    capsule_out=$(grep -vE "⚠️|Auto-provisioning|Provision|Using CPython|Creating virtual|Audited|Metrics|Dangerous" "$out" | head -1)
    if [ -n "$capsule_out" ] && ! grep -qiE "E[0-9]{3,}:|×|error:|failed" "$out"; then
        pass "Basic capsule run succeeds on $(uname -srm): $capsule_out"
    else
        fail "Basic capsule run on $(uname -srm)" "$(tail -5 "$out")"
    fi
    rm -rf "$dir"
}

# ---------------------------------------------------------------------------
# Automated: minimum supported macOS version check
# ---------------------------------------------------------------------------
test_macos_min_version() {
    if [ "$(uname)" != "Darwin" ]; then
        skip "macOS version check (not macOS)"
        return
    fi
    local ver
    ver=$(sw_vers -productVersion 2>/dev/null || echo "0.0.0")
    local major minor
    major=$(echo "$ver" | cut -d. -f1)
    minor=$(echo "$ver" | cut -d. -f2)
    info "macOS version: $ver"
    if [ "$major" -ge 13 ]; then
        pass "macOS $ver is within supported range (≥13 Ventura)"
    elif [ "$major" -ge 12 ]; then
        pass "macOS $ver (12 Monterey) — check release notes for minimum supported version"
    else
        fail "macOS version" "macOS $ver may be below minimum supported version"
    fi
}

# ---------------------------------------------------------------------------
# Automated: Apple Silicon + Rosetta check (macOS)
# ---------------------------------------------------------------------------
test_apple_silicon_rosetta() {
    if [ "$(uname)" != "Darwin" ]; then skip "Apple Silicon Rosetta check (macOS-only)"; return; fi
    local arch
    arch=$(uname -m)
    if [ "$arch" = "arm64" ]; then
        info "Apple Silicon detected (arm64)"
        if /usr/bin/arch -x86_64 true 2>/dev/null; then
            info "Rosetta 2 is installed"
            pass "Apple Silicon — Rosetta 2 available for x86_64 compatibility if needed"
        else
            pass "Apple Silicon — Rosetta 2 not installed (native arm64 capsules should not need it)"
        fi
    else
        skip "Apple Silicon Rosetta check (Intel Mac — not applicable)"
    fi
}

# ---------------------------------------------------------------------------
# Automated: NixOS — ato binary runs (FHS compat)
# ---------------------------------------------------------------------------
test_nixos_fhs() {
    if [ ! -f /etc/NIXOS ] && ! grep -qi "NixOS" /etc/os-release 2>/dev/null; then
        skip "NixOS FHS test (not NixOS)"
        return
    fi
    info "Running on NixOS — checking FHS compatibility"
    local out="$ATO_TEST_TMP/nixos_fhs.txt"
    if run_cmd 10 "$out" ato --version; then
        pass "ato binary runs on NixOS (FHS wrapper or patchelf applied correctly)"
    else
        if grep -qi "No such file\|cannot execute\|ELF\|interpreter" "$out"; then
            fail "ato on NixOS" "FHS binary issue: $(tail -3 "$out") — wrap with nix-ld or patchelf"
        else
            fail "ato on NixOS" "$(tail -3 "$out")"
        fi
    fi
}

# ---------------------------------------------------------------------------
# Automated: iCloud/OneDrive/Dropbox home — capsule.toml parses correctly
# ---------------------------------------------------------------------------
test_cloud_synced_home() {
    local synced=false
    local sync_dir=""
    for d in "$HOME/Library/Mobile Documents/com~apple~CloudDocs" "$HOME/OneDrive" "$HOME/Dropbox"; do
        [ -d "$d" ] && synced=true && sync_dir="$d" && break
    done
    if ! $synced; then
        skip "Cloud-synced home test — no cloud sync directory found"
        return
    fi
    info "Cloud sync directory: $sync_dir"
    local dir="$sync_dir/ato-test-$$"
    mkdir -p "$dir"
    cat > "$dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "cloud-sync-test"
version = "0.1.0"
type = "app"
run = "python3 -c 'print(\"cloud ok\")'"
runtime = "source/python"
EOF
    local out="$ATO_TEST_TMP/cloud_sync.txt"
    provision_python_capsule "$dir"
    run_cmd 30 "$out" env CAPSULE_ALLOW_UNSAFE=1 ato run --dangerously-skip-permissions "$dir" || true
    if grep -q "cloud ok" "$out"; then
        pass "Capsule runs from cloud-synced home directory ($sync_dir)"
    else
        # Distinguish file-lock/cloud-agent interference from other errors
        if grep -qiE "locked by|resource busy|device busy|file.*locked|sync.*conflict" "$out"; then
            fail "Cloud-synced home" "File lock conflict with sync agent: $(tail -3 "$out")"
        else
            fail "Cloud-synced home" "$(tail -3 "$out")"
        fi
    fi
    rm -rf "$dir"
}

# ---------------------------------------------------------------------------
# Automated: Windows 10 (detect and warn)
# ---------------------------------------------------------------------------
test_windows10_support() {
    if [ "$(uname)" = "Darwin" ] || [ "$(uname)" = "Linux" ]; then
        skip "Windows 10 check (not Windows)"
        return
    fi
    local win_ver
    win_ver=$(cmd.exe /c ver 2>/dev/null | grep -oE "10\.[0-9]+\.[0-9]+\.[0-9]+" | head -1)
    if [ -n "$win_ver" ]; then
        info "Windows version: $win_ver"
        pass "Windows 10 detected — confirm ato release notes list Win10 as supported"
    fi
}

# ---------------------------------------------------------------------------
# Human: ARM Linux server (AWS Graviton / Raspberry Pi)
# ---------------------------------------------------------------------------
test_arm_linux() {
    if uname -m | grep -qi "aarch64\|arm"; then
        checklist "ARM Linux (this machine is ARM — run these checks)" \
            "Run: ato run <sample-capsule> — confirm it works natively on arm64" \
            "No Rosetta / QEMU emulation required for standard capsules" \
            "ato binary is the arm64 build (not emulated x86_64)" \
            "Confirm GPU passthrough works if this is AWS Graviton (inferentia/trainium if applicable)"
    else
        skip "ARM Linux test — current machine is $(uname -m)"
    fi
}

# ---------------------------------------------------------------------------
# Human: MDM-managed Mac (Jamf/Kandji)
# ---------------------------------------------------------------------------
test_mdm_managed_mac() {
    checklist "Enterprise MDM-managed Mac (Jamf / Kandji)" \
        "On a Mac enrolled in MDM, launch ato-desktop" \
        "Confirm MDM profile does not block ~/.ato/ directory creation" \
        "Confirm MDM does not quarantine ato binary (check /var/log/system.log for MDM block events)" \
        "Confirm SIP (System Integrity Protection) does not interfere with ato sandboxing" \
        "FileVault enabled — ato works normally on encrypted home directory"
}

# ---------------------------------------------------------------------------
# Human: BitLocker / LUKS encrypted home
# ---------------------------------------------------------------------------
test_encrypted_home() {
    checklist "Encrypted home directory (FileVault / BitLocker / LUKS)" \
        "On a machine with encrypted home, run ato after full unlock" \
        "Capsule runs normally — no key/permission errors from encrypted filesystem" \
        "~/.ato/keys/ stores signing keys correctly on encrypted volume" \
        "After reboot + unlock, ato runs without requiring re-initialization" \
        "FileVault (macOS): no issues with XPC services accessing encrypted ~/.ato/"
}

# ---------------------------------------------------------------------------
# Human: Old macOS minimum support
# ---------------------------------------------------------------------------
test_old_macos() {
    checklist "Old macOS — minimum supported version" \
        "Check the release notes for the declared minimum macOS version" \
        "On a machine running the minimum macOS version, install and run ato" \
        "ato binary launches without 'requires newer OS' error from Gatekeeper" \
        "All features work (no API usage that requires a newer OS than declared minimum)" \
        "On macOS older than minimum: ato shows a clear 'unsupported OS' error at launch"
}

test_environment_fingerprint
test_basic_run_this_platform
test_macos_min_version
test_apple_silicon_rosetta
test_nixos_fhs
test_cloud_synced_home
test_windows10_support
test_arm_linux
test_mdm_managed_mac
test_encrypted_home
test_old_macos

print_suite_summary "$SUITE"
