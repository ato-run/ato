#!/bin/bash
# ─────────────────────────────────────────────────────────────────────────────
# docker_no_runtime_runner.sh
# Runs INSIDE the Docker tester container.
# Asserts that ato manages its own Node runtime without any host runtimes.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

PASS=0
FAIL=0

pass() { echo "PASS: $1"; PASS=$((PASS + 1)); }
fail() { echo "FAIL: $1"; FAIL=$((FAIL + 1)); }

# ─── Test 1: Confirm no host runtimes ─────────────────────────────────────────
echo "=== Test 1: No host runtimes on PATH ==="
if which node 2>/dev/null; then
    fail "host 'node' found on PATH ($(which node))"
else
    pass "no host 'node' on PATH"
fi

if which npm 2>/dev/null; then
    fail "host 'npm' found on PATH ($(which npm))"
else
    pass "no host 'npm' on PATH"
fi

if which python3 2>/dev/null; then
    fail "host 'python3' found on PATH ($(which python3))"
else
    pass "no host 'python3' on PATH"
fi

# ─── Test 2: npm provider — managed Node is downloaded and used ────────────────
# npm:semver has a CLI binary; `semver 1.0.0` prints `1.0.0` and exits 0.
# Version suffix (@7) is not supported in MVP mode, so use the bare package name.
echo ""
echo "=== Test 2: npm:semver via managed Node ==="
SEMVER_OUTPUT=$(ato run npm:semver --yes -- 1.0.0 2>&1) || {
    fail "ato run npm:semver exited non-zero; output: $SEMVER_OUTPUT"
    SEMVER_OUTPUT=""
}
if [ -n "$SEMVER_OUTPUT" ] && echo "$SEMVER_OUTPUT" | grep -q "1\.0\.0"; then
    pass "npm:semver output contains '1.0.0': $SEMVER_OUTPUT"
else
    fail "npm:semver unexpected output: ${SEMVER_OUTPUT:-<empty>}"
fi

# ─── Test 3: source/node capsule — npm run script uses managed Node ────────────
# Uses `run = "npm run start"` which routes through build_package_manager_command
# (the managed Node path, not Deno). The npm start script calls `node --version`
# to confirm managed Node 20.x is on PATH (the #294 fix).
echo ""
echo "=== Test 3: source/node capsule npm run script with managed Node (#294) ==="
CAPSULE_DIR=$(mktemp -d)
cat > "$CAPSULE_DIR/capsule.toml" << 'EOF'
schema_version = "0.3"
name = "path-fix-test"
version = "0.1.0"
type = "app"
runtime = "source/node"
runtime_version = "20.11.0"
run = "npm run start"
EOF

cat > "$CAPSULE_DIR/package.json" << 'EOF'
{
  "name": "path-fix-test",
  "version": "0.1.0",
  "private": true,
  "scripts": {
    "start": "node -e \"const v=process.version;console.log('NODE_VERSION='+v);if(!v.startsWith('v20.')){process.exit(1);}console.log('PATH_TEST=OK');\""
  }
}
EOF

# Pre-supply a lockfile so the auto-provisioner does not trigger a shadow run.
cat > "$CAPSULE_DIR/package-lock.json" << 'EOF'
{
  "name": "path-fix-test",
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

if echo "$CAPSULE_OUTPUT" | grep -q "PATH_TEST=OK"; then
    pass "source/node npm run script used managed Node 20.x (#294)"
    echo "  $(echo "$CAPSULE_OUTPUT" | grep NODE_VERSION | head -1)"
else
    fail "source/node npm run script PATH_TEST not OK; output: $CAPSULE_OUTPUT"
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

