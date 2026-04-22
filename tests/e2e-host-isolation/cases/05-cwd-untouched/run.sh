#!/usr/bin/env bash
# Test 05: User cwd untouched
#
# Runs ato in a fresh empty directory and asserts it leaves no artifacts behind.
# Guarantees ato does not write node_modules, .ato/, package.json, or anything
# else to the user's working directory.
set -euo pipefail
source "$(dirname "$0")/../../harness/assert.sh"

SENTINEL=$(mktemp -d)
trap "rm -rf $SENTINEL" EXIT

(cd "$SENTINEL" && ato run npm:semver --yes -- 1.0.0 > /dev/null 2>&1)

assert_dir_empty "$SENTINEL" \
  "user cwd was not polluted after ato run npm:semver"
