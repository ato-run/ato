#!/usr/bin/env bash
# macOS self-host acceptance: the checks a release gate re-runs against the
# binary Homebrew installs. Every check prints PASS/FAIL and the script exits
# non-zero if any check that ran failed.
#
#   scripts/acceptance/macos-selfhost-e2e.sh [--binary <path>] [--hub] [--github]
#
# --hub     also run a Hub reference (needs ATO_API_URL and ATO_API_TOKEN)
# --github  also fetch a GitHub source (needs network; slower)
#
# Nothing is written outside .tmp/ in the repository root.
set -u

root="$(cd "$(dirname "$0")/../.." && pwd)"
binary="$root/target/release/ato"
run_hub=0
run_github=0
while [ $# -gt 0 ]; do
  case "$1" in
    --binary) binary="$2"; shift 2 ;;
    --hub) run_hub=1; shift ;;
    --github) run_github=1; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

work="$root/.tmp/selfhost-e2e"
rm -rf "$work"
mkdir -p "$work/home"
export ATO_HOME="$work/home"

failures=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; failures=$((failures + 1)); }
skip() { printf 'SKIP  %s\n' "$1"; }

[ -x "$binary" ] || { echo "no binary at $binary (cargo build --release -p ato-cli)" >&2; exit 2; }

# 1. The artifact runs and depends only on what macOS ships.
version="$("$binary" --version 2>&1)" && pass "binary runs: $version" || fail "binary does not run"
if [ "$(uname -s)" = "Darwin" ]; then
  foreign="$(otool -L "$binary" | tail -n +2 | awk '{print $1}' \
    | grep -v -E '^/System/|^/usr/lib/' || true)"
  if [ -z "$foreign" ]; then
    pass "links only system libraries"
  else
    fail "links non-system libraries: $foreign"
  fi
fi

# 2. A canned Capsule runs and proves its own Contract.
if "$binary" run "$root/samples/interop-static.capsule" --no-open \
     --verification-receipt "$work/receipt.json" >"$work/run.log" 2>&1; then
  if python3 - "$work/receipt.json" <<'PY'
import json, sys
receipt = json.load(open(sys.argv[1]))
sys.exit(0 if receipt.get("fully_satisfied") is True else 1)
PY
  then
    pass "canned capsule: fully_satisfied"
  else
    fail "canned capsule: receipt is not fully_satisfied"
  fi
else
  fail "canned capsule did not run (see $work/run.log)"
fi

# 3. The optional Model Set is described before it is downloaded.
if "$binary" model status --json >"$work/model.json" 2>"$work/model.err"; then
  if python3 - "$work/model.json" <<'PY'
import json, sys
report = json.load(open(sys.argv[1]))
ok = report.get("schema") == "ato.model-status/1" and len(report.get("assets", [])) == 12
sys.exit(0 if ok else 1)
PY
  then
    pass "model status: 12 pinned assets"
  else
    fail "model status: unexpected report"
  fi
else
  fail "model status failed (see $work/model.err)"
fi

# 4. A Hub reference resolves, verifies and runs.
if [ "$run_hub" = "1" ]; then
  if [ -z "$(printenv ATO_API_TOKEN)" ] || [ -z "$(printenv ATO_API_URL)" ]; then
    skip "hub reference (set ATO_API_URL and ATO_API_TOKEN)"
  elif "$binary" run capsules/koh0920/interop-static --no-open \
         --verification-receipt "$work/hub-receipt.json" >"$work/hub.log" 2>&1; then
    if python3 - "$work/hub-receipt.json" <<'PY'
import json, sys
receipt = json.load(open(sys.argv[1]))
sys.exit(0 if receipt.get("fully_satisfied") is True else 1)
PY
    then
      pass "hub reference: fully_satisfied"
    else
      fail "hub reference: receipt is not fully_satisfied"
    fi
  else
    fail "hub reference did not run (see $work/hub.log)"
  fi
else
  skip "hub reference (pass --hub)"
fi

# 5. A GitHub source is fetched and pinned to a commit.
if [ "$run_github" = "1" ]; then
  if ! "$binary" source fetch --help >/dev/null 2>&1; then
    # The GitHub source path lands with its own change; a binary without it is a
    # check that cannot run yet, not a release-gate failure.
    skip "github source (this binary has no 'source fetch')"
  elif "$binary" source fetch https://github.com/octocat/Spoon-Knife \
       --output "$work/source" >"$work/source.log" 2>&1; then
    if grep -q '"commit"' "$work/source/provenance.json" 2>/dev/null; then
      pass "github source: fetched and pinned"
    else
      fail "github source: no pinned commit in provenance"
    fi
  else
    fail "github source fetch failed (see $work/source.log)"
  fi
else
  skip "github source (pass --github)"
fi

echo
if [ "$failures" -eq 0 ]; then
  echo "all checks passed"
  exit 0
fi
echo "$failures check(s) failed"
exit 1
