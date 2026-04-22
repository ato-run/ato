#!/usr/bin/env bash
# Test 03: Wrong Python version on host
#
# Guarantees ato uses managed Python even when host has a different version.
# NOTE: PythonProvisioner is not yet fully implemented (until-pythonprovisioner-v0.5.x).
# This test is expected to fail and is run with continue-on-error in CI.
set -euo pipefail
source "$(dirname "$0")/../../harness/assert.sh"

HOST_PY=$(python3 --version 2>/dev/null || echo "MISSING")
echo "  host python: $HOST_PY"

OUTPUT=$(ato run pypi:rich --yes -- --version 2>&1)
echo "$OUTPUT"

# Check that host Python version string does NOT appear in ato output
# (ato should use its managed Python, not the host one)
if [ "$HOST_PY" != "MISSING" ]; then
  # Extract the host version number (e.g. "3.9.x") from "Python 3.9.x"
  HOST_PY_VER=$(echo "$HOST_PY" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' || echo "")
  if [ -n "$HOST_PY_VER" ]; then
    assert_not_contains "$OUTPUT" "Python $HOST_PY_VER" \
      "ato did not use host Python $HOST_PY_VER (isolation intact)"
  fi
fi

assert_not_contains "$OUTPUT" "ERROR" \
  "pypi:rich ran without errors"
