#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# docker_wrong_runtime_runner.sh
# Runs INSIDE the Docker tester container (node:22-bookworm-slim base).
# Asserts that ato uses its managed Node runtime even when a different Node
# version is already present on the host PATH.
#
# Regression test for #294: "managed Node 20 should be used even when host
# has Node 22 (or any other wrong version) installed".
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

PASS=0
FAIL=0

pass() { echo "PASS: $1"; PASS=$((PASS + 1)); }
fail() { echo "FAIL: $1"; FAIL=$((FAIL + 1)); }

# ─── Test 1: Confirm host Node is present and on PATH ─────────────────────────
echo "=== Test 1: Host Node is reachable on PATH (wrong version) ==="
HOST_NODE_VERSION=$(node --version 2>/dev/null || echo "")
if [ -z "$HOST_NODE_VERSION" ]; then
    fail "Expected host node to exist in this image but 'node' not found"
else
    pass "Host node found: $HOST_NODE_VERSION"
    echo "  host node: $HOST_NODE_VERSION (should be v22.x)"
fi

# ─── Test 2: npm provider — managed Node is used, not the host Node ──────────
# ato run npm:semver should download & use managed Node 20, ignoring the host.
echo ""
echo "=== Test 2: npm:semver uses managed Node (not host Node 22) ==="
SEMVER_OUTPUT=$(ato run npm:semver --yes -- 1.2.3 2>&1) || {
    fail "ato run npm:semver exited non-zero; output: $SEMVER_OUTPUT"
    SEMVER_OUTPUT=""
}
if echo "$SEMVER_OUTPUT" | grep -q "1\.2\.3"; then
    pass "npm:semver output contains '1.2.3': OK"
else
    fail "npm:semver unexpected output: ${SEMVER_OUTPUT:-<empty>}"
fi

# ─── Test 3: source/node capsule — child process sees managed Node 20 ─────────
# This is the core #294 regression test: not just that ato picks managed Node
# for its own process, but that the child process spawned by `npm run start`
# also sees managed Node when it shells out to `node`.
echo ""
echo "=== Test 3: source/node child process PATH resolves managed Node 20 (#294) ==="
CAPSULE_DIR=$(mktemp -d)
cat > "$CAPSULE_DIR/capsule.toml" << 'EOF'
schema_version = "0.3"
name = "wrong-runtime-path-test"
version = "0.1.0"
type = "app"
runtime = "source/node"
runtime_version = "20.11.0"
run = "npm run start"
EOF

cat > "$CAPSULE_DIR/package.json" << 'EOF'
{
  "name": "wrong-runtime-path-test",
  "version": "0.1.0",
  "private": true,
  "scripts": {
    "start": "node -e \"const v=process.version; console.log('CHILD_NODE_VERSION=' + v); if (!v.startsWith('v20.')) { console.error('ERROR: expected v20.x but got ' + v); process.exit(1); } console.log('CHILD_PATH_TEST=OK');\""
  }
}
EOF

cat > "$CAPSULE_DIR/package-lock.json" << 'EOF'
{
  "name": "wrong-runtime-path-test",
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

if echo "$CAPSULE_OUTPUT" | grep -q "CHILD_PATH_TEST=OK"; then
    pass "source/node child process saw managed Node 20.x (#294 regression)"
    echo "  $(echo "$CAPSULE_OUTPUT" | grep CHILD_NODE_VERSION | head -1)"
else
    fail "source/node child process did NOT use managed Node 20; output: $CAPSULE_OUTPUT"
fi

rm -rf "$CAPSULE_DIR"

# ─── Test 4: `which node` still returns host Node (ato does not mutate PATH globally) ─
# ato must NOT mutate the user's shell PATH. The host node should still be
# the one returned by `which node` outside of an ato-managed child process.
echo ""
echo "=== Test 4: ato run does not permanently mutate shell PATH ==="
HOST_NODE_AFTER=$(which node 2>/dev/null || echo "")
if [ -n "$HOST_NODE_AFTER" ] && echo "$HOST_NODE_AFTER" | grep -q "node"; then
    pass "shell PATH unchanged after ato run (host node still at $HOST_NODE_AFTER)"
else
    fail "unexpected 'which node' after ato run: ${HOST_NODE_AFTER:-<empty>}"
fi

# ─── Test 5: User cwd is not polluted even when host has wrong Node version ────
echo ""
echo "=== Test 5: User cwd is not polluted when host has wrong Node ==="
SENTINEL_DIR=$(mktemp -d)
BEFORE=$(find "$SENTINEL_DIR" -maxdepth 3 2>/dev/null | sort)

(cd "$SENTINEL_DIR" && ato run npm:semver --yes -- 0.1.0 >/dev/null 2>&1) || true

AFTER=$(find "$SENTINEL_DIR" -maxdepth 3 2>/dev/null | sort)

CWD_POLLUTED=0
[ -d "$SENTINEL_DIR/node_modules" ] && { fail "node_modules created in user cwd"; CWD_POLLUTED=1; }
[ -f "$SENTINEL_DIR/package.json" ] && { fail "package.json created in user cwd"; CWD_POLLUTED=1; }
[ -d "$SENTINEL_DIR/.ato" ] && { fail ".ato/ created in user cwd"; CWD_POLLUTED=1; }
[ "$BEFORE" != "$AFTER" ] && [ "$CWD_POLLUTED" -eq 0 ] && { fail "user cwd was modified"; CWD_POLLUTED=1; }
[ "$CWD_POLLUTED" -eq 0 ] && pass "user cwd untouched after ato run npm:semver (wrong-runtime env)"

rm -rf "$SENTINEL_DIR"

# ─── Summary ──────────────────────────────────────────────────────────────────
echo ""
echo "====================================="
echo "Results: $PASS passed, $FAIL failed"
echo "====================================="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
