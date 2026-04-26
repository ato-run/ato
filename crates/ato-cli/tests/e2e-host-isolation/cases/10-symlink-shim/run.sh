#!/usr/bin/env bash
# Test 10: Symlink shim follow
#
# Some version managers (asdf, mise) place symlinks in PATH directories that
# point to shim scripts. When the symlink target echoes a poison marker, ato
# must not follow and execute the shim.
set -euo pipefail
source "$(dirname "$0")/../../harness/assert.sh"

SHIM_DIR=$(mktemp -d)
REAL_BIN="$SHIM_DIR/node-real-$$"
POISON_MARKER="SYMLINK-SHIM-POISONED-$$"

cat > "$REAL_BIN" << EOF
#!/bin/sh
echo "$POISON_MARKER"
EOF
chmod +x "$REAL_BIN"

# Place a symlink named "node" pointing at the poison binary in a temp dir,
# then prepend that dir to PATH so the shim would be found before any real node.
SHIM_LINK_DIR=$(mktemp -d)
ln -sf "$REAL_BIN" "$SHIM_LINK_DIR/node"

cleanup() { rm -rf "$SHIM_DIR" "$SHIM_LINK_DIR"; }
trap cleanup EXIT

OUTPUT=$(PATH="$SHIM_LINK_DIR:$PATH" ato run npm:semver --yes -- 2.0.0 2>&1)
echo "$OUTPUT"

assert_not_contains "$OUTPUT" "$POISON_MARKER" \
  "ato did not follow symlink shim from $SHIM_LINK_DIR"

assert_contains "$OUTPUT" "2.0.0" \
  "npm:semver produced expected output"
