#!/usr/bin/env bash
# Test 09: macOS path_helper trap
#
# /usr/libexec/path_helper reads /etc/paths.d/* and prepends those directories
# to PATH when invoked (which happens automatically in login shells).
# If ato uses a login shell to launch child processes, path_helper will source
# any entry in /etc/paths.d/ and inject attacker-controlled directories.
# This test verifies ato is immune to this macOS-specific trap.
set -euo pipefail
source "$(dirname "$0")/../../harness/assert.sh"

POISON_PATHS_D="/etc/paths.d/ato-test09-poison"
POISON_DIR=$(mktemp -d)
POISON_MARKER="PATHS-D-POISONED-NODE-$$"

cat > "$POISON_DIR/node" << EOF
#!/bin/sh
echo "$POISON_MARKER"
EOF
chmod +x "$POISON_DIR/node"

echo "$POISON_DIR" | sudo tee "$POISON_PATHS_D" > /dev/null

cleanup() {
  sudo rm -f "$POISON_PATHS_D" 2>/dev/null || true
  rm -rf "$POISON_DIR"
}
trap cleanup EXIT

# Sanity: confirm path_helper picks up the poison dir in a login shell
PATH_HELPER_OUTPUT=$(bash -lc 'echo "$PATH"' 2>/dev/null || true)
assert_contains "$PATH_HELPER_OUTPUT" "$POISON_DIR" \
  "sanity: path_helper injected poison dir into login-shell PATH"

OUTPUT=$(ato run npm:semver --yes -- 3.0.0 2>&1)
echo "$OUTPUT"

assert_not_contains "$OUTPUT" "$POISON_MARKER" \
  "ato did not trigger path_helper /etc/paths.d injection"

assert_contains "$OUTPUT" "3.0.0" \
  "npm:semver produced expected output"
