#!/usr/bin/env bash
# Test 07: sh -lc profile.d trap (Linux/macOS)
#
# Executable assertion for RFC UNIFIED_EXECUTION_MODEL §4.2:
# "ato MUST NOT use 'sh -lc' or 'bash -lc' to invoke managed runtimes."
#
# If ato ever spawns child processes via a login shell, /etc/profile.d scripts
# will be sourced and can inject arbitrary directories at the front of PATH.
# This test poisons /etc/profile.d with a script that prepends a directory
# containing a fake npm binary, then verifies ato's output is clean.
set -euo pipefail
source "$(dirname "$0")/../../harness/assert.sh"

POISON_PROFILE="/etc/profile.d/ato-test07-poison.sh"
POISON_BIN_DIR=$(mktemp -d)
POISON_MARKER="PROFILE-POISONED-NPM-$$"

cat > "$POISON_BIN_DIR/npm" << EOF
#!/bin/sh
echo "$POISON_MARKER"
EOF
chmod +x "$POISON_BIN_DIR/npm"

sudo tee "$POISON_PROFILE" > /dev/null << EOF
export PATH="$POISON_BIN_DIR:\$PATH"
EOF

cleanup() {
  sudo rm -f "$POISON_PROFILE" 2>/dev/null || true
  rm -rf "$POISON_BIN_DIR"
}
trap cleanup EXIT

# Sanity: confirm the profile.d trap works for login shells
LOGIN_NPM=$(bash -lc 'npm' 2>/dev/null || true)
assert_contains "$LOGIN_NPM" "$POISON_MARKER" \
  "sanity: login-shell npm is poisoned (trap is active)"

# Now verify ato does NOT go through a login shell
OUTPUT=$(ato run npm:semver --yes -- 3.0.0 2>&1)
echo "$OUTPUT"

assert_not_contains "$OUTPUT" "$POISON_MARKER" \
  "ato did not invoke profile.d poison (sh -lc guard intact)"

assert_contains "$OUTPUT" "3.0.0" \
  "npm:semver produced expected output"
