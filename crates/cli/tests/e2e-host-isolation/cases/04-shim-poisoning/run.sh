#!/usr/bin/env bash
# Test 04: Shim poisoning
#
# Places a fake "node" binary early on PATH (in a directory ato cannot
# be allowed to inherit) and asserts that ato never calls it.
# Covers: asdf/mise/nvm shims in /usr/local/bin and /opt/homebrew/bin.
set -euo pipefail
source "$(dirname "$0")/../../harness/assert.sh"

case "$(uname -s)" in
  Darwin) POISON_DIR="/opt/homebrew/bin" ;;
  Linux)  POISON_DIR="/usr/local/bin" ;;
  *)      echo "Unsupported OS: $(uname -s)"; exit 1 ;;
esac

POISON_MARKER="SHIM-POISONED-NODE-$$"
POISON_BIN="$POISON_DIR/node-poison-$$"

sudo tee "$POISON_BIN" > /dev/null << EOF
#!/bin/sh
echo "$POISON_MARKER"
EOF
sudo chmod +x "$POISON_BIN"
sudo ln -sf "$POISON_BIN" "$POISON_DIR/node"

cleanup() {
  sudo rm -f "$POISON_BIN" "$POISON_DIR/node" 2>/dev/null || true
}
trap cleanup EXIT

OUTPUT=$(ato run npm:semver --yes -- 2.0.0 2>&1)
echo "$OUTPUT"

assert_not_contains "$OUTPUT" "$POISON_MARKER" \
  "ato did not invoke poisoned node from $POISON_DIR"

assert_contains "$OUTPUT" "2.0.0" \
  "npm:semver produced expected output"
