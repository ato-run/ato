#!/usr/bin/env bash
# Test 02: Wrong Node version on host
#
# Guarantees ato uses managed Node (20.11.0) even when host has a different
# version already on PATH (e.g. Node 22 from Homebrew on macOS CI, or the
# GitHub runner's default Node on Linux).
set -euo pipefail
source "$(dirname "$0")/../../harness/assert.sh"

# Pre-condition: host must have node (CI sets this up before calling this script)
HOST_NODE=$(node --version 2>/dev/null || echo "MISSING")
assert_not_equal "$HOST_NODE" "MISSING" \
  "pre-condition: host node must exist on PATH"
echo "  host node: $HOST_NODE"

CAPSULE_DIR=$(mktemp -d)
trap "rm -rf $CAPSULE_DIR" EXIT

cat > "$CAPSULE_DIR/capsule.toml" << 'EOF'
schema_version = "0.3"
name = "test02-wrong-node"
version = "0.1.0"
type = "app"
runtime = "source/node"
runtime_version = "20.11.0"
run = "npm run check"
EOF

cat > "$CAPSULE_DIR/package.json" << 'EOF'
{
  "name": "test02-wrong-node",
  "version": "0.1.0",
  "private": true,
  "scripts": {
    "check": "node -e \"const v=process.version; console.log('MANAGED_NODE=' + v); if (!v.startsWith('v20.')) { console.error('WRONG_NODE=' + v); process.exit(1); }\""
  }
}
EOF

cat > "$CAPSULE_DIR/package-lock.json" << 'EOF'
{"name":"test02-wrong-node","version":"0.1.0","lockfileVersion":3,"requires":true,"packages":{}}
EOF

OUTPUT=$(ato run --yes "$CAPSULE_DIR" 2>&1)
echo "$OUTPUT"

assert_contains "$OUTPUT" "MANAGED_NODE=v20." \
  "ato used managed Node 20.x (host was $HOST_NODE)"

# Host PATH must not be permanently mutated
HOST_NODE_AFTER=$(node --version 2>/dev/null || echo "MISSING")
assert_equal "$HOST_NODE_AFTER" "$HOST_NODE" \
  "ato did not mutate host PATH after run"
