#!/usr/bin/env bash
# Test 06: Child process spawned by npm inherits managed PATH
#
# Verifies that not just ato's own process but also child processes spawned
# by npm scripts see the managed Node binary (not the host one).
# Regression guard for the class of bugs where ato sets managed PATH for
# itself but npm's child node still resolves to the host.
set -euo pipefail
source "$(dirname "$0")/../../harness/assert.sh"

CAPSULE_DIR=$(mktemp -d)
trap "rm -rf $CAPSULE_DIR" EXIT

cat > "$CAPSULE_DIR/capsule.toml" << 'EOF'
schema_version = "0.3"
name = "test06-child-spawn"
version = "0.1.0"
type = "app"
runtime = "source/node"
runtime_version = "20.11.0"
run = "npm run check-child"
EOF

cat > "$CAPSULE_DIR/package.json" << 'EOF'
{
  "name": "test06-child-spawn",
  "version": "0.1.0",
  "private": true,
  "scripts": {
    "check-child": "node -e \"const v=process.version; console.log('CHILD_NODE=' + v); if (!v.startsWith('v20.')) { console.error('WRONG_CHILD_NODE=' + v); process.exit(1); }\""
  }
}
EOF

cat > "$CAPSULE_DIR/package-lock.json" << 'EOF'
{"name":"test06-child-spawn","version":"0.1.0","lockfileVersion":3,"requires":true,"packages":{}}
EOF

OUTPUT=$(ato run --yes "$CAPSULE_DIR" 2>&1)
echo "$OUTPUT"

assert_contains "$OUTPUT" "CHILD_NODE=v20." \
  "child process spawned by npm saw managed Node 20.x"
