#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# docker_sh_lc_trap_runner.sh
# Runs INSIDE the Docker tester container (login-shell PATH trap via profile.d).
# Asserts that ato never sources /etc/profile.d/ when launching child processes.
#
# Executable assertion for RFC UNIFIED_EXECUTION_MODEL §4.2:
# "ato MUST NOT use 'sh -lc' or 'bash -lc' to invoke managed runtimes."
#
# If ato uses a login shell to launch node/npm, the /etc/profile.d/wrong-path.sh
# script will prepend /opt/wrong/bin to PATH, causing the poison shims to be
# called instead of the managed binaries.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

PASS=0
FAIL=0

pass() { echo "PASS: $1"; PASS=$((PASS + 1)); }
fail() { echo "FAIL: $1"; FAIL=$((FAIL + 1)); }

# ─── Test 1: Confirm profile.d trap works for login shells (setup sanity) ─────
echo "=== Test 1: Login shell trap is active ==="
LOGIN_NPM=$(bash -lc 'npm' 2>/dev/null || true)
if echo "$LOGIN_NPM" | grep -q "PROFILE-POISONED-NPM"; then
    pass "login shell trap is active (bash -lc npm outputs poison)"
else
    fail "profile.d trap not working — test environment may be wrong: ${LOGIN_NPM:-<empty>}"
fi

# ─── Test 2: ato does not source profile.d — npm:semver must not poison ───────
echo ""
echo "=== Test 2: npm:semver output is not poisoned by profile.d ==="
SEMVER_OUTPUT=$(ato run npm:semver --yes -- 3.0.0 2>&1) || {
    fail "ato run npm:semver exited non-zero; output: $SEMVER_OUTPUT"
    SEMVER_OUTPUT=""
}
if echo "$SEMVER_OUTPUT" | grep -q "PROFILE-POISONED"; then
    fail "ato npm:semver triggered profile.d poison: $SEMVER_OUTPUT"
else
    pass "ato npm:semver did not trigger profile.d trap"
fi
if echo "$SEMVER_OUTPUT" | grep -q "3\.0\.0"; then
    pass "npm:semver produced expected output '3.0.0'"
else
    fail "npm:semver did not produce expected '3.0.0'; output: ${SEMVER_OUTPUT:-<empty>}"
fi

# ─── Test 3: source/node capsule child — profile.d must not be sourced ────────
echo ""
echo "=== Test 3: source/node capsule does not source profile.d (§4.2) ==="
CAPSULE_DIR=$(mktemp -d)
cat > "$CAPSULE_DIR/capsule.toml" << 'EOF'
schema_version = "0.3"
name = "sh-lc-trap-test"
version = "0.1.0"
type = "app"
runtime = "source/node"
runtime_version = "20.11.0"
run = "npm run start"
EOF

# The npm script checks whether /opt/wrong/bin is in the child's PATH.
# If ato used sh -lc, /opt/wrong/bin would appear there.
cat > "$CAPSULE_DIR/package.json" << 'EOF'
{
  "name": "sh-lc-trap-test",
  "version": "0.1.0",
  "private": true,
  "scripts": {
    "start": "node -e \"const p=process.env.PATH||''; if(p.includes('/opt/wrong/bin')){console.error('FAIL: profile.d was sourced, PATH=' + p); process.exit(1);} console.log('SH_LC_SAFE=OK'); console.log('NODE_VERSION=' + process.version);\""
  }
}
EOF

cat > "$CAPSULE_DIR/package-lock.json" << 'EOF'
{
  "name": "sh-lc-trap-test",
  "version": "0.1.0",
  "lockfileVersion": 3,
  "requires": true,
  "packages": {}
}
EOF

CAPSULE_OUTPUT=$(ato run --yes "$CAPSULE_DIR" 2>&1) || {
    fail "source/node capsule exited non-zero; output: $CAPSULE_OUTPUT"
    CAPSULE_OUTPUT=""
}

if echo "$CAPSULE_OUTPUT" | grep -q "SH_LC_SAFE=OK"; then
    pass "child process PATH does not contain /opt/wrong/bin (profile.d not sourced)"
    echo "  $(echo "$CAPSULE_OUTPUT" | grep NODE_VERSION | head -1)"
else
    fail "capsule may have sourced profile.d; output: ${CAPSULE_OUTPUT:-<empty>}"
fi

rm -rf "$CAPSULE_DIR"

# ─── Summary ──────────────────────────────────────────────────────────────────
echo ""
echo "====================================="
echo "Results: $PASS passed, $FAIL failed"
echo "====================================="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
