#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# docker_host_python_leakage_runner.sh
# Runs INSIDE the Docker tester container (python:3.9-slim base).
# Asserts that ato uses its managed Python (3.12.x) even when host has Python 3.9.
#
# NOTE: This test is expected to FAIL until PythonProvisioner is implemented.
# It serves as an executable specification ("failing test = missing feature").
# When PythonProvisioner lands in v0.5.x, this test should go green automatically.
#
# Corresponding Rust ignore annotation:
#   #[ignore = "until-pythonprovisioner-v0.5.x"]
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

PASS=0
FAIL=0

pass() { echo "PASS: $1"; PASS=$((PASS + 1)); }
fail() { echo "FAIL: $1 [until-pythonprovisioner-v0.5.x]"; FAIL=$((FAIL + 1)); }

# ─── Test 1: Confirm host Python 3.9 is present ───────────────────────────────
echo "=== Test 1: Host Python 3.9 is present (wrong version) ==="
HOST_PY_VERSION=$(python3 --version 2>/dev/null || echo "")
if echo "$HOST_PY_VERSION" | grep -q "3\.9\."; then
    pass "Host Python found: $HOST_PY_VERSION"
else
    fail "Expected Python 3.9 from base image but got: ${HOST_PY_VERSION:-<none>}"
fi

# ─── Test 2: pypi:rich uses managed Python 3.12, not host Python 3.9 ─────────
# [until-pythonprovisioner-v0.5.x] This will fail until PythonProvisioner
# downloads and manages its own Python runtime independent of the host.
echo ""
echo "=== Test 2: pypi:rich uses managed Python 3.12 (not host 3.9) ==="
echo "  NOTE: This test is expected to FAIL until PythonProvisioner v0.5.x"

RICH_OUTPUT=$(ato run pypi:rich --yes -- --version 2>&1) || {
    fail "ato run pypi:rich exited non-zero; output: $RICH_OUTPUT"
    RICH_OUTPUT=""
}

# Check that the Python version used is 3.12.x, not host 3.9.x.
# This assertion will fail until PythonProvisioner manages its own Python.
if echo "$RICH_OUTPUT" | grep -qE "3\.12\.[0-9]"; then
    pass "pypi:rich used managed Python 3.12.x"
elif echo "$RICH_OUTPUT" | grep -qE "3\.9\.[0-9]"; then
    fail "pypi:rich used HOST Python 3.9 instead of managed 3.12 (leakage bug)"
else
    fail "pypi:rich output does not show expected Python version; output: ${RICH_OUTPUT:-<empty>}"
fi

# ─── Test 3: Child process PATH does not expose host Python ───────────────────
echo ""
echo "=== Test 3: Managed Python child sees managed Python version (not 3.9) ==="
echo "  NOTE: This test is expected to FAIL until PythonProvisioner v0.5.x"

CAPSULE_DIR=$(mktemp -d)
cat > "$CAPSULE_DIR/capsule.toml" << 'EOF'
schema_version = "0.3"
name = "python-leakage-test"
version = "0.1.0"
type = "app"
runtime = "source/python"
runtime_version = "3.12"
run = "main.py"
EOF

cat > "$CAPSULE_DIR/main.py" << 'EOF'
import sys
v = sys.version_info
print(f"PYTHON_VERSION={v.major}.{v.minor}.{v.micro}")
if v.major != 3 or v.minor != 12:
    print(f"ERROR: expected Python 3.12 but got {v.major}.{v.minor}")
    sys.exit(1)
print("PYTHON_VERSION_OK")
EOF

CAPSULE_OUTPUT=$(ato run --yes "$CAPSULE_DIR" 2>&1) || {
    fail "source/python capsule exited non-zero; output: $CAPSULE_OUTPUT"
    CAPSULE_OUTPUT=""
}

if echo "$CAPSULE_OUTPUT" | grep -q "PYTHON_VERSION_OK"; then
    pass "source/python capsule used managed Python 3.12"
    echo "  $(echo "$CAPSULE_OUTPUT" | grep PYTHON_VERSION= | head -1)"
else
    fail "source/python capsule did NOT use managed Python 3.12; output: ${CAPSULE_OUTPUT:-<empty>}"
fi

rm -rf "$CAPSULE_DIR"

# ─── Summary ──────────────────────────────────────────────────────────────────
echo ""
echo "====================================="
echo "Results: $PASS passed, $FAIL failed"
echo "  (failures expected until PythonProvisioner v0.5.x)"
echo "====================================="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
