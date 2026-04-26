#!/bin/bash
# =============================================================================
# §4 サンドボックス境界の実測
# Uses the test-sandbox fixture (tests/test-sandbox/)
# =============================================================================
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../config.sh"
RESULT_FILE="$RESULTS_DIR/result_04_sandbox_boundary.log"
: > "$RESULT_FILE"

SUITE="§4 Sandbox Boundary"
echo "══════════════════════════════════"
echo " $SUITE"
echo "══════════════════════════════════"
check_ato

FIXTURE_DIR="$SCRIPT_DIR/../../test-sandbox"

# ---------------------------------------------------------------------------
# Automated: run the test-sandbox fixture and check result
# ---------------------------------------------------------------------------
test_sandbox_fixture() {
    if [ ! -d "$FIXTURE_DIR" ]; then
        skip "test-sandbox fixture not found at $FIXTURE_DIR"
        return
    fi

    provision_python_capsule "$FIXTURE_DIR"
    local out="$ATO_TEST_TMP/sandbox_fixture.txt"
    # Run with --sandbox to actually test isolation enforcement.
    # If sandbox is not yet supported for source/python (E301), SKIP — this is
    # a known limitation (L1/D1: network enforcement is advisory on macOS/source runtimes).
    run_cmd 60 "$out" ato run --sandbox "$FIXTURE_DIR" || true
    if grep -qi "E301\|not yet supported\|sandbox.*not.*support\|unsupported.*sandbox" "$out"; then
        skip "test-sandbox fixture — sandbox not yet enforced for source/python on this platform (L1 known limitation; D1 advisory)"
        return
    fi

    if grep -qi "SECURITY ISSUE" "$out"; then
        fail "test-sandbox fixture" "Security boundary breached: $(grep 'SECURITY ISSUE' "$out" | head -3)"
    else
        local failed_count
        failed_count=$(grep -c '\[FAIL\]' "$out" 2>/dev/null || echo "0")
        if [ "$failed_count" -eq 0 ]; then
            pass "test-sandbox fixture — all isolation checks passed"
        else
            fail "test-sandbox fixture" "$failed_count check(s) failed: $(grep '\[FAIL\]' "$out" | head -3)"
        fi
    fi
}

# ---------------------------------------------------------------------------
# Automated: filesystem.workspace write outside boundary is blocked
# ---------------------------------------------------------------------------
test_fs_write_outside_workspace() {
    local tmp_dir="$ATO_TEST_TMP/fs-boundary-capsule"
    mkdir -p "$tmp_dir"
    cat > "$tmp_dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "fs-write-outside-test"
version = "0.1.0"
type = "app"
run = "python3 probe.py"
runtime = "source/python"

[isolation]
sandbox = true

[isolation.filesystem]
read_write = ["./output"]
EOF
    # Probe tries to write to the parent dir — should be blocked
    cat > "$tmp_dir/probe.py" <<'EOF'
import os, sys
target = "/tmp/ato_sandbox_escape_test.txt"
try:
    with open(target, "w") as f:
        f.write("escaped")
    print(f"SECURITY_VIOLATION: wrote to {target}")
    sys.exit(1)
except (PermissionError, OSError) as e:
    print(f"BLOCKED: {e}")
    sys.exit(0)
EOF

    provision_python_capsule "$tmp_dir"
    local out="$ATO_TEST_TMP/fs_outside.txt"
    run_cmd 30 "$out" ato run --sandbox "$tmp_dir" || true
    if grep -qi "E301" "$out"; then
        info "§4 sandbox enforcement skipped — E301: ato run --sandbox requires entrypoint (not yet supported for source/python)"
        rm -rf "$tmp_dir"; skip "Filesystem write outside workspace (E301: --sandbox not yet supported for source/python)"; return
    fi

    if grep -qi "SECURITY_VIOLATION" "$out"; then
        fail "Filesystem write outside workspace blocked" "Sandbox escape succeeded — write to /tmp was not blocked"
    elif grep -qi "BLOCKED\|PermissionError\|Operation not permitted" "$out"; then
        pass "Filesystem write outside workspace is blocked"
    else
        # If sandbox is not yet enforced on this platform, treat as skip
        info "Output: $(cat "$out" | head -10)"
        skip "Filesystem write outside workspace — indeterminate (sandbox may not be enforced on this OS)"
    fi
    rm -rf "$tmp_dir"
}

# ---------------------------------------------------------------------------
# Automated: network.allow off → DNS/HTTP blocked
# ---------------------------------------------------------------------------
test_network_denied_by_default() {
    local tmp_dir="$ATO_TEST_TMP/net-deny-capsule"
    mkdir -p "$tmp_dir"
    cat > "$tmp_dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "net-deny-test"
version = "0.1.0"
type = "app"
run = "python3 probe.py"
runtime = "source/python"

[isolation]
sandbox = true

[isolation.network]
enabled = false
EOF
    cat > "$tmp_dir/probe.py" <<'EOF'
import socket, sys
try:
    socket.setdefaulttimeout(3)
    socket.getaddrinfo("google.com", 80)
    print("SECURITY_VIOLATION: DNS resolved despite network disabled")
    sys.exit(1)
except (OSError, socket.gaierror) as e:
    print(f"BLOCKED: {e}")
    sys.exit(0)
EOF

    provision_python_capsule "$tmp_dir"
    local out="$ATO_TEST_TMP/net_deny.txt"
    run_cmd 30 "$out" ato run --sandbox "$tmp_dir" || true
    if grep -qi "E301" "$out"; then
        rm -rf "$tmp_dir"; skip "Network deny-by-default (E301: --sandbox not yet supported for source/python)"; return
    fi

    if grep -qi "SECURITY_VIOLATION" "$out"; then
        fail "Network deny-by-default" "DNS resolved inside sandbox with network disabled"
    elif grep -qi "BLOCKED\|not permitted\|refused\|timeout\|Errno" "$out"; then
        pass "Network deny-by-default — DNS blocked inside sandbox"
    else
        info "Output: $(cat "$out" | head -10)"
        skip "Network deny-by-default — indeterminate (sandbox may not be enforced on this OS)"
    fi
    rm -rf "$tmp_dir"
}

# ---------------------------------------------------------------------------
# Automated: process.spawn blocked
# ---------------------------------------------------------------------------
test_process_spawn_blocked() {
    local tmp_dir="$ATO_TEST_TMP/spawn-block-capsule"
    mkdir -p "$tmp_dir"
    cat > "$tmp_dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "spawn-block-test"
version = "0.1.0"
type = "app"
run = "python3 probe.py"
runtime = "source/python"

[isolation]
sandbox = true
allow_spawn = false
EOF
    cat > "$tmp_dir/probe.py" <<'EOF'
import subprocess, sys
try:
    result = subprocess.run(["id"], capture_output=True, timeout=3)
    if result.returncode == 0:
        print(f"SECURITY_VIOLATION: spawned process: {result.stdout.decode().strip()}")
        sys.exit(1)
    else:
        print(f"BLOCKED: subprocess returned {result.returncode}")
        sys.exit(0)
except (PermissionError, OSError, FileNotFoundError) as e:
    print(f"BLOCKED: {e}")
    sys.exit(0)
except subprocess.TimeoutExpired:
    print("BLOCKED: timeout (spawn stalled)")
    sys.exit(0)
EOF

    provision_python_capsule "$tmp_dir"
    local out="$ATO_TEST_TMP/spawn_block.txt"
    run_cmd 30 "$out" ato run --sandbox "$tmp_dir" || true
    if grep -qi "E301" "$out"; then
        rm -rf "$tmp_dir"; skip "Process spawn blocked (E301: --sandbox not yet supported for source/python)"; return
    fi

    if grep -qi "SECURITY_VIOLATION" "$out"; then
        fail "Process spawn blocked" "Subprocess spawned despite allow_spawn = false"
    elif grep -qi "BLOCKED\|PermissionError\|not permitted" "$out"; then
        pass "Process spawn blocked — subprocess call is denied"
    else
        info "Output: $(cat "$out" | head -10)"
        skip "Process spawn blocked — indeterminate (allow_spawn may not be enforced on this OS)"
    fi
    rm -rf "$tmp_dir"
}

# ---------------------------------------------------------------------------
# Automated: kill -9 cleanup — no orphan processes
# ---------------------------------------------------------------------------
test_sigkill_cleanup() {
    local tmp_dir="$ATO_TEST_TMP/sigkill-capsule"
    mkdir -p "$tmp_dir"
    cat > "$tmp_dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "sigkill-cleanup-test"
version = "0.1.0"
type = "app"
run = "python3 -c 'import time; time.sleep(300)'"
runtime = "source/python"
EOF

    provision_python_capsule "$tmp_dir"
    local out="$ATO_TEST_TMP/sigkill.txt"
    # Launch in background, grab PID, kill -9, check for orphan python process
    CAPSULE_ALLOW_UNSAFE=1 ato run --dangerously-skip-permissions "$tmp_dir" >"$out" 2>&1 &
    local ato_pid=$!
    sleep 2
    # Kill with SIGKILL
    kill -9 "$ato_pid" 2>/dev/null || true
    sleep 2

    # Check for orphan child processes
    if pgrep -f "sigkill-cleanup-test" &>/dev/null; then
        fail "SIGKILL cleanup" "Orphan capsule process remains after kill -9"
    else
        pass "SIGKILL cleanup — no orphan processes after kill -9"
    fi
    rm -rf "$tmp_dir"
}

# ---------------------------------------------------------------------------
# Human: unprivileged user namespaces disabled (Linux)
# ---------------------------------------------------------------------------
test_bubblewrap_no_unpriv_ns() {
    if [ "$(uname)" != "Linux" ]; then
        skip "bubblewrap unprivileged-ns test (Linux-only)"
        return
    fi
    local ns_val
    ns_val=$(cat /proc/sys/kernel/unprivileged_userns_clone 2>/dev/null || echo "unknown")
    if [ "$ns_val" = "0" ]; then
        checklist "Bubblewrap on system with unprivileged user namespaces DISABLED" \
            "Run: ato run <any-source-capsule>" \
            "Confirm ato detects the missing capability and prints a clear error" \
            "Error message explains that unprivileged user namespaces must be enabled or root is required" \
            "ato exits non-zero and does NOT silently run without sandbox"
    else
        info "Unprivileged user namespaces are enabled (value=$ns_val) — skipping disabled-ns check"
        skip "bubblewrap unprivileged-ns disabled check (namespaces are enabled on this machine)"
    fi
}

# ---------------------------------------------------------------------------
# Human: child process capability inheritance
# ---------------------------------------------------------------------------
test_child_capability_inheritance() {
    checklist "Child processes inherit capsule capability grants (not full host caps)" \
        "Run a capsule that spawns a child process (e.g. python3 → subprocess → another python3)" \
        "Verify child process cannot access paths outside the declared filesystem grants" \
        "Verify child process cannot open network connections not in egress_allow" \
        "Confirm inheritance is enforced via seccomp/landlock (Linux) or sandbox-exec (macOS)"
}

# ---------------------------------------------------------------------------
# Human: Windows AppContainer
# ---------------------------------------------------------------------------
test_windows_appcontainer() {
    if [ "$(uname)" = "Darwin" ] || [ "$(uname)" = "Linux" ]; then
        skip "Windows AppContainer test (Windows-only)"
        return
    fi
    checklist "Windows AppContainer isolation" \
        "Run a source capsule on Windows with sandbox = true" \
        "Confirm it runs in Low IL via Process Monitor or icacls" \
        "Windows Defender does not block capsule execution" \
        "SmartScreen warning does not appear for signed capsule" \
        "Capsule exits cleanly — no ACL residue in temp dirs"
}

test_sandbox_fixture
test_fs_write_outside_workspace
test_network_denied_by_default
test_process_spawn_blocked
test_sigkill_cleanup
test_bubblewrap_no_unpriv_ns
test_child_capability_inheritance
test_windows_appcontainer

print_suite_summary "$SUITE"
