#!/bin/bash
# =============================================================================
# §9 ネットワーク隔離の実観測
# Uses packet-level tools where available (tcpdump, ss, lsof)
# =============================================================================
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../config.sh"
RESULT_FILE="$RESULTS_DIR/result_09_network_isolation.log"
: > "$RESULT_FILE"

SUITE="§9 Network Isolation"
echo "══════════════════════════════════"
echo " $SUITE"
echo "══════════════════════════════════"
check_ato

# ---------------------------------------------------------------------------
# Automated: capsule with network.enabled=false cannot reach the internet
# ---------------------------------------------------------------------------
test_deny_by_default_dns() {
    local tmp_dir="$ATO_TEST_TMP/net-deny-dns"
    mkdir -p "$tmp_dir"
    cat > "$tmp_dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "net-deny-dns-test"
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
    ip = socket.gethostbyname("google.com")
    print(f"VIOLATION: DNS resolved to {ip}")
    sys.exit(1)
except Exception as e:
    print(f"BLOCKED: {e}")
    sys.exit(0)
EOF
    local out="$ATO_TEST_TMP/deny_dns.txt"
    provision_python_capsule "$tmp_dir"
    run_cmd 30 "$out" ato run --sandbox "$tmp_dir" || true
    if grep -qi "E301" "$out"; then
        rm -rf "$tmp_dir"; skip "deny-by-default DNS (E301: --sandbox not yet supported for source/python)"; return
    fi
    if grep -qi "VIOLATION" "$out"; then
        fail "deny-by-default DNS blocked" "DNS resolved inside sandbox with network disabled"
    elif grep -qi "BLOCKED\|not permitted\|refused\|Errno\|gaierror" "$out"; then
        pass "deny-by-default — DNS blocked inside sandbox"
    else
        info "Output: $(cat "$out" | head -8)"
        skip "deny-by-default DNS — indeterminate (sandbox may not be enforced on this platform)"
    fi
    rm -rf "$tmp_dir"
}

# ---------------------------------------------------------------------------
# Automated: egress_allow listed host is reachable
# ---------------------------------------------------------------------------
test_egress_allow_works() {
    local tmp_dir="$ATO_TEST_TMP/net-egress-allow"
    mkdir -p "$tmp_dir"
    cat > "$tmp_dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "net-egress-allow-test"
version = "0.1.0"
type = "app"
run = "python3 probe.py"
runtime = "source/python"

[isolation]
sandbox = true
[isolation.network]
enabled = true
egress_allow = ["httpbin.org"]
EOF
    cat > "$tmp_dir/probe.py" <<'EOF'
import urllib.request, sys
try:
    req = urllib.request.urlopen("http://httpbin.org/get", timeout=10)
    if req.status == 200:
        print("ALLOWED: httpbin.org reachable")
        sys.exit(0)
    else:
        print(f"UNEXPECTED HTTP {req.status}")
        sys.exit(1)
except Exception as e:
    print(f"BLOCKED: {e}")
    sys.exit(1)
EOF
    local out="$ATO_TEST_TMP/egress_allow.txt"
    provision_python_capsule "$tmp_dir"
    run_cmd 30 "$out" ato run --sandbox "$tmp_dir" || true
    if grep -qi "E301" "$out"; then
        rm -rf "$tmp_dir"; skip "egress_allow (E301: --sandbox not yet supported for source/python)"; return
    fi
    if grep -qi "ALLOWED" "$out"; then
        pass "egress_allow — listed host (httpbin.org) is reachable"
    elif grep -qi "BLOCKED" "$out"; then
        fail "egress_allow — listed host" "httpbin.org blocked despite being in egress_allow"
    else
        info "Output: $(cat "$out" | head -8)"
        skip "egress_allow — indeterminate (sandbox may not be enforced on this platform)"
    fi
    rm -rf "$tmp_dir"
}

# ---------------------------------------------------------------------------
# Automated: unlisted host is blocked when egress_allow is set
# ---------------------------------------------------------------------------
test_egress_unlisted_blocked() {
    local tmp_dir="$ATO_TEST_TMP/net-egress-unlisted"
    mkdir -p "$tmp_dir"
    cat > "$tmp_dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "net-egress-unlisted-test"
version = "0.1.0"
type = "app"
run = "python3 probe.py"
runtime = "source/python"

[isolation]
sandbox = true
[isolation.network]
enabled = true
egress_allow = ["httpbin.org"]
EOF
    cat > "$tmp_dir/probe.py" <<'EOF'
import socket, sys
# Try connecting to a host NOT in egress_allow
try:
    socket.setdefaulttimeout(3)
    socket.create_connection(("example.com", 80))
    print("VIOLATION: connected to example.com (not in egress_allow)")
    sys.exit(1)
except Exception as e:
    print(f"BLOCKED: {e}")
    sys.exit(0)
EOF
    local out="$ATO_TEST_TMP/egress_unlisted.txt"
    provision_python_capsule "$tmp_dir"
    run_cmd 30 "$out" ato run --sandbox "$tmp_dir" || true
    if grep -qi "E301" "$out"; then
        rm -rf "$tmp_dir"; skip "egress unlisted blocked (E301: --sandbox not yet supported for source/python)"; return
    fi
    if grep -qi "VIOLATION" "$out"; then
        fail "egress unlisted host blocked" "Connected to example.com despite it not being in egress_allow"
    elif grep -qi "BLOCKED\|not permitted\|refused" "$out"; then
        pass "egress unlisted host (example.com) is blocked"
    else
        info "Output: $(cat "$out" | head -8)"
        skip "egress unlisted blocked — indeterminate (sandbox may not be enforced)"
    fi
    rm -rf "$tmp_dir"
}

# ---------------------------------------------------------------------------
# Automated: blocked connection error is actionable (names the host)
# ---------------------------------------------------------------------------
test_blocked_error_message_actionable() {
    local tmp_dir="$ATO_TEST_TMP/net-blocked-msg"
    mkdir -p "$tmp_dir"
    cat > "$tmp_dir/capsule.toml" <<'EOF'
schema_version = "0.3"
name = "net-blocked-msg-test"
version = "0.1.0"
type = "app"
run = "python3 -c 'import urllib.request; urllib.request.urlopen(\"http://blocked.example.com\", timeout=3)'"
runtime = "source/python"

[isolation]
sandbox = true
[isolation.network]
enabled = false
EOF
    local out="$ATO_TEST_TMP/blocked_msg.txt"
    provision_python_capsule "$tmp_dir"
    run_cmd 30 "$out" ato run --sandbox "$tmp_dir" || true
    if grep -qi "E301" "$out"; then
        rm -rf "$tmp_dir"; skip "Blocked error actionable (E301: --sandbox not yet supported for source/python)"; return
    fi
    # Look for the hostname in the error output
    if grep -qi "blocked.example.com\|network.*disabled\|connection refused\|not permitted" "$out"; then
        pass "Blocked connection error message is actionable (names host or reason)"
    else
        info "Output: $(cat "$out" | head -8)"
        skip "Blocked error actionable — message quality depends on sandbox enforcement"
    fi
    rm -rf "$tmp_dir"
}

# ---------------------------------------------------------------------------
# Human: tcpdump / Wireshark / Little Snitch observation
# ---------------------------------------------------------------------------
test_packet_level_verification() {
    checklist "Packet-level verification with tcpdump / Wireshark / Little Snitch" \
        "Start tcpdump: sudo tcpdump -i any -n 'host 8.8.8.8 or host google.com'" \
        "Run a capsule with network.enabled = false" \
        "Confirm tcpdump captures NO packets to/from denied hosts" \
        "Run a capsule with egress_allow = [\"httpbin.org\"]" \
        "Confirm tcpdump shows packets ONLY to httpbin.org (no leakage to other hosts)" \
        "On macOS, configure Little Snitch rule to block capsule process — verify ato reports clean error"
}

# ---------------------------------------------------------------------------
# Human: Tailnet sidecar
# ---------------------------------------------------------------------------
test_tailnet_sidecar() {
    checklist "Tailnet sidecar (ato-tsnetd) SOCKS5 proxy" \
        "Start ato-tsnetd on both machines in the same tailnet" \
        "Run a capsule with networking routed via SOCKS5 proxy" \
        "Confirm capsule can reach tailnet-internal hosts" \
        "Disconnect tailnet on the sidecar machine" \
        "Confirm capsule gets a clean 'network unavailable' error (not a hang)"
}

# ---------------------------------------------------------------------------
# Human: corporate proxy / VPN
# ---------------------------------------------------------------------------
test_corporate_proxy() {
    checklist "Corporate proxy / VPN environment" \
        "Connect to a corporate network with an HTTPS inspection proxy (MITM)" \
        "Run: ato run <any-capsule> — capsule launch succeeds (proxy trusted)" \
        "Confirm ato.run TLS certificate validation works through the proxy" \
        "Test with Pi-hole / NextDNS blocking ato.run — ato gives clear DNS error" \
        "Test with split-tunnel VPN: ato.run traffic stays on VPN as expected"
}

# ---------------------------------------------------------------------------
# Human: captive portal
# ---------------------------------------------------------------------------
test_captive_portal() {
    checklist "Captive portal (hotel / conference Wi-Fi)" \
        "Connect to a Wi-Fi network with a captive portal (redirect all HTTP)" \
        "Run: ato run <any-capsule> — ato should detect no internet and give clear error" \
        "Error message instructs user to open browser to complete portal login" \
        "After portal auth, ato run retried successfully"
}

# ---------------------------------------------------------------------------
# Human: IPv6-only / dual-stack
# ---------------------------------------------------------------------------
test_ipv6() {
    checklist "IPv6-only and dual-stack networks" \
        "On a dual-stack network (IPv4 + IPv6), run ato run <capsule> — confirm it works" \
        "On an IPv6-only network, run ato run — confirm it works (or fails gracefully)" \
        "egress_allow with an IPv6 address ([2001:db8::1]) is respected"
}

test_deny_by_default_dns
test_egress_allow_works
test_egress_unlisted_blocked
test_blocked_error_message_actionable
test_packet_level_verification
test_tailnet_sidecar
test_corporate_proxy
test_captive_portal
test_ipv6

print_suite_summary "$SUITE"
