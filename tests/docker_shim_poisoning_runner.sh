#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# docker_shim_poisoning_runner.sh
# Runs INSIDE the Docker tester container (shim-poisoned /usr/local/bin).
# Asserts that ato never invokes the poison shims when running npm:semver or
# a source/node capsule.
#
# This covers the class of bugs where version managers (asdf, mise, nvm,
# pyenv, rbenv) write shims to directories that appear early on PATH.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

PASS=0
FAIL=0

pass() { echo "PASS: $1"; PASS=$((PASS + 1)); }
fail() { echo "FAIL: $1"; FAIL=$((FAIL + 1)); }

# ─── Test 1: Confirm shims are reachable (setup sanity) ───────────────────────
echo "=== Test 1: Shim binaries are on PATH ==="
NODE_SHIM_OUTPUT=$(node 2>/dev/null || true)
if echo "$NODE_SHIM_OUTPUT" | grep -q "SHIM-POISONED-NODE"; then
    pass "node shim is active on PATH: $NODE_SHIM_OUTPUT"
else
    fail "node shim not found or not outputting expected string: ${NODE_SHIM_OUTPUT:-<empty>}"
fi

# ─── Test 2: npm provider — shim output must NOT appear in ato output ─────────
echo ""
echo "=== Test 2: npm:semver output contains no shim poison ==="
SEMVER_OUTPUT=$(ato run npm:semver --yes -- 2.0.0 2>&1) || {
    fail "ato run npm:semver exited non-zero; output: $SEMVER_OUTPUT"
    SEMVER_OUTPUT=""
}
if echo "$SEMVER_OUTPUT" | grep -q "SHIM-POISONED"; then
    fail "npm:semver output contains poison shim string: $SEMVER_OUTPUT"
else
    pass "npm:semver output is clean (no shim poison)"
fi
if echo "$SEMVER_OUTPUT" | grep -q "2\.0\.0"; then
    pass "npm:semver produced expected output '2.0.0'"
else
    fail "npm:semver did not produce expected '2.0.0'; output: ${SEMVER_OUTPUT:-<empty>}"
fi

# ─── Test 3: source/node capsule — child process must not call shim ───────────
echo ""
echo "=== Test 3: source/node capsule child process bypasses node shim ==="
CAPSULE_DIR=$(mktemp -d)
cat > "$CAPSULE_DIR/capsule.toml" << 'EOF'
schema_version = "0.3"
name = "shim-poisoning-test"
version = "0.1.0"
type = "app"
runtime = "source/node"
runtime_version = "20.11.0"
run = "npm run start"
EOF

cat > "$CAPSULE_DIR/package.json" << 'EOF'
{
  "name": "shim-poisoning-test",
  "version": "0.1.0",
  "private": true,
  "scripts": {
    "start": "node -e \"const v=process.version; if(v.includes('SHIM-POISONED')){process.exit(1);} console.log('MANAGED_VERSION=' + v); if(!v.startsWith('v20.')){console.error('wrong version: '+v); process.exit(1);} console.log('SHIM_BYPASS_OK');\""
  }
}
EOF

cat > "$CAPSULE_DIR/package-lock.json" << 'EOF'
{
  "name": "shim-poisoning-test",
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

if echo "$CAPSULE_OUTPUT" | grep -q "SHIM-POISONED"; then
    fail "capsule output contains shim poison: $CAPSULE_OUTPUT"
elif echo "$CAPSULE_OUTPUT" | grep -q "SHIM_BYPASS_OK"; then
    pass "source/node child process bypassed node shim and used managed Node 20"
    echo "  $(echo "$CAPSULE_OUTPUT" | grep MANAGED_VERSION | head -1)"
else
    fail "unexpected capsule output: ${CAPSULE_OUTPUT:-<empty>}"
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
