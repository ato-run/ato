#!/usr/bin/env bash
# Test 05: User cwd untouched
#
# Runs ato in a fresh empty directory and asserts it leaves no artifacts behind.
# Guarantees ato does not write node_modules, .ato/, package.json, or anything
# else to the user's working directory.
#
# Case A: provider-backed scheme (npm:semver) — never wrote to cwd
# Case B: directory project (ato run .) — previously wrote attempt-<nanos> dirs
#         under .ato/tmp/source-inference/; fixed by USE_HOME_RUN_STATE = true
set -euo pipefail
source "$(dirname "$0")/../../harness/assert.sh"

# ── Case A: provider-backed ──────────────────────────────────────────────────
SENTINEL=$(mktemp -d)
trap "rm -rf $SENTINEL" EXIT

(cd "$SENTINEL" && ato run npm:semver --yes -- 1.0.0 > /dev/null 2>&1)

assert_dir_empty "$SENTINEL" \
  "user cwd was not polluted after ato run npm:semver"

# ── Case B: directory project ─────────────────────────────────────────────────
# Create a minimal directory project and run ato against it.
# Regardless of whether the run succeeds (Python may not be available),
# run-attempt state must NOT appear under the project's .ato/tmp/ tree.
PROJECT=$(mktemp -d)
trap "rm -rf $PROJECT" EXIT

cat > "$PROJECT/main.py" << 'PYEOF'
print('hello')
PYEOF

cat > "$PROJECT/capsule.toml" << 'TOMLEOF'
schema_version = "0.3"
name = "test-cwd-isolation"
version = "0.1.0"
type = "app"
run = "python main.py"
runtime = "source/python"
TOMLEOF

# Run ato; ignore exit code — we only care about filesystem side-effects
(cd "$PROJECT" && ato run . --yes > /dev/null 2>&1) || true

ENTRIES_UNDER_TMP=$(find "$PROJECT/.ato/tmp/source-inference" -type f 2>/dev/null | wc -l | tr -d ' ')
assert_equal "$ENTRIES_UNDER_TMP" "0" \
  "directory project: cwd .ato/tmp/source-inference/ was polluted"
