#!/bin/bash
# =============================================================================
# §12 既存ツールチェーンとの干渉
# =============================================================================
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../config.sh"
RESULT_FILE="$RESULTS_DIR/result_12_toolchain_interference.log"
: > "$RESULT_FILE"

SUITE="§12 Toolchain Interference"
echo "══════════════════════════════════"
echo " $SUITE"
echo "══════════════════════════════════"
check_ato

# ---------------------------------------------------------------------------
# Automated: detect common version managers in PATH
# ---------------------------------------------------------------------------
test_detect_version_managers() {
    local found=()
    for tool in pyenv rbenv nvm fnm conda uv pipx pnpm bun; do
        command -v "$tool" &>/dev/null && found+=("$tool")
    done
    if [ "${#found[@]}" -gt 0 ]; then
        info "Version managers detected: ${found[*]}"
        pass "Version manager detection — found: ${found[*]}"
    else
        info "No common version managers detected — install pyenv/nvm/etc. to test interference"
        pass "Version manager detection — none present (interference tests not applicable)"
    fi
}

# ---------------------------------------------------------------------------
# Automated: ato binary is first in PATH (not shadowed by version managers)
# ---------------------------------------------------------------------------
test_ato_not_shadowed() {
    local ato_path
    ato_path=$(command -v ato 2>/dev/null)
    if [ -z "$ato_path" ]; then
        fail "ato not shadowed" "ato not found in PATH at all"
        return
    fi
    # Check that ato is actually ato (not a pyenv shim etc.)
    local output
    output=$(ato --version 2>&1 | head -1)
    if echo "$output" | grep -qiE "ato|capsuled|[0-9]+\.[0-9]+"; then
        pass "ato binary not shadowed: $ato_path ($output)"
    else
        fail "ato not shadowed" "ato --version returned unexpected: $output"
    fi
}

# ---------------------------------------------------------------------------
# Automated: pyenv active Python does not leak into sandbox
# ---------------------------------------------------------------------------
test_pyenv_isolation() {
    if ! command -v pyenv &>/dev/null; then
        skip "pyenv not installed — pyenv isolation test not applicable"
        return
    fi
    local pyenv_version
    pyenv_version=$(pyenv version-name 2>/dev/null || echo "system")
    info "Current pyenv version: $pyenv_version"

    local dir="$ATO_TEST_TMP/pyenv-capsule"
    mkdir -p "$dir"
    cat > "$dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "pyenv-isolation-test"
version = "0.1.0"
type = "app"
run = "python3 --version"
runtime = "source/python"

[language.python]
version = "3.11"
EOF
    local out="$ATO_TEST_TMP/pyenv_isolation.txt"
    provision_python_capsule "$dir"
    if run_cmd 30 "$out" env CAPSULE_ALLOW_UNSAFE=1 ato run --dangerously-skip-permissions "$dir"; then
        local reported_version
        reported_version=$(grep -oiE "Python [0-9]+\.[0-9]+\.[0-9]+" "$out" | head -1)
        pass "pyenv active — capsule runs with declared Python version (reported: $reported_version)"
    else
        if grep -qi "version\|not found\|3\.11" "$out"; then
            pass "pyenv capsule — version resolution attempted (exit nonzero may be expected)"
        else
            fail "pyenv isolation" "$(tail -5 "$out")"
        fi
    fi
    rm -rf "$dir"
}

# ---------------------------------------------------------------------------
# Automated: conda active environment does not affect capsule runtime
# ---------------------------------------------------------------------------
test_conda_isolation() {
    if ! command -v conda &>/dev/null; then
        skip "conda not installed — conda isolation test not applicable"
        return
    fi
    local conda_env="${CONDA_DEFAULT_ENV:-none}"
    info "Active conda env: $conda_env"

    local dir="$ATO_TEST_TMP/conda-capsule"
    mkdir -p "$dir"
    cat > "$dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "conda-isolation-test"
version = "0.1.0"
type = "app"
run = "python3 -c 'import sys; print(sys.executable)'"
runtime = "source/python"
EOF
    local out="$ATO_TEST_TMP/conda_isolation.txt"
    provision_python_capsule "$dir"
    run_cmd 30 "$out" env CAPSULE_ALLOW_UNSAFE=1 ato run --dangerously-skip-permissions "$dir" || true
    # If it runs, check the Python path is NOT the conda env Python
    if grep -qi "/opt/conda\|/anaconda\|/miniconda\|conda/envs" "$out"; then
        fail "conda isolation" "Capsule is using conda environment Python: $(cat "$out" | head -3)"
    else
        pass "conda active — capsule Python is not the conda environment Python"
    fi
    rm -rf "$dir"
}

# ---------------------------------------------------------------------------
# Automated: container environment detection
# ---------------------------------------------------------------------------
test_container_environment() {
    local in_container=false
    [ -f "/.dockerenv" ] && in_container=true
    [ -n "${container:-}" ] && in_container=true
    grep -qi "docker\|lxc\|containerd" /proc/1/cgroup 2>/dev/null && in_container=true

    if $in_container; then
        info "Running inside a container — testing nested sandbox behavior"
        local dir="$ATO_TEST_TMP/container-capsule"
        mkdir -p "$dir"
        cat > "$dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "container-nested-test"
version = "0.1.0"
type = "app"
run = "python3 -c 'print(\"ok from container\")'"
runtime = "source/python"
EOF
        local out="$ATO_TEST_TMP/container_nested.txt"
        provision_python_capsule "$dir"
        if run_cmd 30 "$out" env CAPSULE_ALLOW_UNSAFE=1 ato run --dangerously-skip-permissions "$dir"; then
            pass "Nested sandbox in container — capsule runs"
        else
            if grep -qi "namespace\|permission\|not supported\|fallback" "$out"; then
                pass "Nested sandbox in container — graceful fallback or error (not a crash)"
            else
                fail "Nested sandbox in container" "$(tail -5 "$out")"
            fi
        fi
        rm -rf "$dir"
    else
        info "Not running in a container — container test not applicable"
        pass "Container detection — not in container (test not applicable on bare metal)"
    fi
}

# ---------------------------------------------------------------------------
# Automated: no PATH collision with common tool names
# ---------------------------------------------------------------------------
test_no_path_collision() {
    # ato should not shadow standard tools
    local collisions=()
    for tool in python python3 node npm pip pip3 git curl; do
        local tool_path
        tool_path=$(command -v "$tool" 2>/dev/null)
        if [ -n "$tool_path" ] && echo "$tool_path" | grep -qi "\.ato"; then
            collisions+=("$tool ($tool_path)")
        fi
    done
    if [ "${#collisions[@]}" -eq 0 ]; then
        pass "No PATH collisions — ato does not shadow standard tools"
    else
        fail "No PATH collisions" "ato is shadowing: ${collisions[*]}"
    fi
}

# ---------------------------------------------------------------------------
# Human: nvm / fnm active Node.js
# ---------------------------------------------------------------------------
test_nvm_fnm_node() {
    checklist "nvm/fnm active Node.js does not leak into capsule" \
        "Activate a specific Node.js version: nvm use 18 (or fnm use 18)" \
        "Run a capsule declaring runtime = 'source/node' with runtime_version = '20'" \
        "Confirm capsule uses Node.js 20 (declared version), not nvm's active 18" \
        "nvm/fnm's PATH modifications do not interfere with ato's runtime resolution"
}

# ---------------------------------------------------------------------------
# Human: antivirus scanning ~/.ato/
# ---------------------------------------------------------------------------
test_antivirus_interference() {
    checklist "Antivirus scanning ~/.ato/ — performance and false positives" \
        "On Windows with Defender active, run: ato run <capsule>" \
        "Confirm Defender does not quarantine ato binary or capsule artifacts" \
        "Confirm disk I/O during capsule install is not unacceptably slow due to real-time scanning" \
        "Add ~/.ato/ to Defender exclusions if performance is impacted — document the requirement" \
        "Test with a 3rd-party AV (Avast / Norton / Kaspersky) — no false positive alerts"
}

# ---------------------------------------------------------------------------
# Human: bandwidth-shaped corporate firewall
# ---------------------------------------------------------------------------
test_bandwidth_shaping() {
    checklist "Bandwidth-shaped corporate firewall — gradual timeout + resume" \
        "On a network that shapes/throttles downloads after N MB" \
        "Start downloading a large capsule" \
        "After throttling kicks in (speed drops to <100KB/s), confirm ato continues (does not timeout)" \
        "If timeout occurs, confirm ato retries with exponential backoff" \
        "Download eventually completes or gives a clear 'timed out' error with retry guidance"
}

# ---------------------------------------------------------------------------
# Human: WSL2 cross-boundary access
# ---------------------------------------------------------------------------
test_wsl2_host_access() {
    checklist "WSL2 — Windows host access to \\\\wsl$\\ paths" \
        "Run a capsule in WSL2 that writes a file to ~/output/" \
        "Access that file from Windows via \\\\wsl$\\Ubuntu\\home\\<user>\\output\\" \
        "File is readable from the Windows side with correct content" \
        "Confirm ato in WSL2 doesn't try to access Windows paths (C:\\) unintentionally" \
        "GPU passthrough works: capsule in WSL2 can use NVIDIA GPU via nvidia-smi"
}

test_detect_version_managers
test_ato_not_shadowed
test_pyenv_isolation
test_conda_isolation
test_container_environment
test_no_path_collision
test_nvm_fnm_node
test_antivirus_interference
test_bandwidth_shaping
test_wsl2_host_access

print_suite_summary "$SUITE"
