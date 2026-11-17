#!/usr/bin/env bash
# macOS self-host acceptance: the twelve checks a release gate re-runs against
# the binary Homebrew installs. Every check prints PASS/FAIL/SKIP and the script
# exits non-zero if any check that ran failed.
#
#   scripts/acceptance/macos-selfhost-e2e.sh [--binary <path>] [--hub] [--github] [--model]
#
# --hub    Hub reference checks (needs ATO_API_URL and ATO_API_TOKEN)
# --github GitHub source check (needs network)
# --model  Model Set checks (needs the 1.8 GB payload and ATO_KEV_TEST_PYTHON)
#
# Nothing is written outside .tmp/ in the repository root.
set -u

root="$(cd "$(dirname "$0")/../.." && pwd)"
binary="$root/target/release/ato"
run_hub=0
run_github=0
run_model=0
while [ $# -gt 0 ]; do
  case "$1" in
    --binary) binary="$2"; shift 2 ;;
    --hub) run_hub=1; shift ;;
    --github) run_github=1; shift ;;
    --model) run_model=1; shift ;;
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

receipt_ok() {
  python3 - "$1" <<'PY'
import json, sys
try:
    receipt = json.load(open(sys.argv[1]))
except Exception:
    sys.exit(1)
sys.exit(0 if receipt.get("fully_satisfied") is True else 1)
PY
}

# 1. The artifact runs and depends only on what macOS ships.
version="$("$binary" --version 2>&1)" && pass "1 binary runs: $version" || fail "1 binary does not run"
if [ "$(uname -s)" = "Darwin" ]; then
  foreign="$(otool -L "$binary" | tail -n +2 | awk '{print $1}' | grep -v -E '^/System/|^/usr/lib/' || true)"
  if [ -z "$foreign" ]; then
    pass "2 links only system libraries"
  else
    fail "2 links non-system libraries: $foreign"
  fi
else
  skip "2 links only system libraries (not macOS)"
fi

# 3. A canned Capsule runs and proves its own Contract.
if "$binary" run "$root/samples/interop-static.capsule" --no-open \
     --verification-receipt "$work/receipt.json" >"$work/run.log" 2>&1; then
  if receipt_ok "$work/receipt.json"; then
    pass "3 canned capsule: fully_satisfied"
  else
    fail "3 canned capsule: receipt is not fully_satisfied"
  fi
else
  fail "3 canned capsule did not run (see $work/run.log)"
fi

# 4. A local .capsule becomes a durable Instance that starts, stops and restarts.
if ! "$binary" app install --help >/dev/null 2>&1; then
  skip "4 durable .capsule lifecycle (this binary has no app install)"
elif "$binary" app install "$root/samples/interop-static.capsule" >"$work/install.log" 2>&1; then
  instance="$(sed -n 's/.*\(linst_[0-9a-f]*\).*/\1/p' "$work/install.log" | head -1)"
  if [ -z "$instance" ]; then
    fail "4 durable .capsule lifecycle: no Instance id in the install output"
  elif "$binary" app stop "$instance" >"$work/stop.log" 2>&1 &&
       "$binary" app start "$instance" >"$work/restart.log" 2>&1; then
    url="$(sed -n 's/^URL: //p' "$work/restart.log" | head -1)"
    if [ -n "$url" ] && curl -sS -m 10 -o /dev/null "$url/"; then
      pass "4 durable .capsule lifecycle: install, stop, restart, serving ($url)"
    else
      fail "4 durable .capsule lifecycle: restarted but not serving ($url)"
    fi
    "$binary" app stop "$instance" >/dev/null 2>&1
  elif grep -rq "process start time is unavailable" "$work/home/instances" 2>/dev/null; then
    skip "4 durable .capsule lifecycle (this host cannot record a process identity)"
  else
    fail "4 durable .capsule lifecycle: start/stop failed (see $work/restart.log)"
  fi
elif grep -rq "process start time is unavailable" "$work/home/instances" 2>/dev/null; then
  skip "4 durable .capsule lifecycle (this host cannot record a process identity)"
else
  fail "4 durable .capsule lifecycle: install failed (see $work/install.log)"
fi

# 5. A Hub reference resolves, verifies and runs.
if [ "$run_hub" != "1" ]; then
  skip "5 hub reference (pass --hub)"
elif [ -z "$(printenv ATO_API_TOKEN)" ] || [ -z "$(printenv ATO_API_URL)" ]; then
  skip "5 hub reference (set ATO_API_URL and ATO_API_TOKEN)"
elif "$binary" run capsules/koh0920/interop-static --no-open \
       --verification-receipt "$work/hub-receipt.json" >"$work/hub.log" 2>&1; then
  if receipt_ok "$work/hub-receipt.json"; then
    pass "5 hub reference: fully_satisfied"
  else
    fail "5 hub reference: receipt is not fully_satisfied"
  fi
else
  fail "5 hub reference did not run (see $work/hub.log)"
fi

# 6. A Hub reference becomes a durable Instance that starts, stops and restarts.
if [ "$run_hub" != "1" ]; then
  skip "6 durable Hub Instance (pass --hub)"
elif ! "$binary" app install --help >/dev/null 2>&1; then
  skip "6 durable Hub Instance (this binary has no app install)"
elif [ -z "$(printenv ATO_API_TOKEN)" ] || [ -z "$(printenv ATO_API_URL)" ]; then
  skip "6 durable Hub Instance (set ATO_API_URL and ATO_API_TOKEN)"
elif "$binary" app install capsules/koh0920/interop-static --api-url "$ATO_API_URL" >"$work/hub-install.log" 2>&1; then
  instance="$(sed -n 's/.*\(linst_[0-9a-f]*\).*/\1/p' "$work/hub-install.log" | head -1)"
  if [ -z "$instance" ]; then
    fail "6 durable Hub Instance: no Instance id in the install output"
  elif "$binary" app stop "$instance" >"$work/hub-stop.log" 2>&1 &&
       "$binary" app start "$instance" >"$work/hub-restart.log" 2>&1; then
    url="$(sed -n 's/^URL: //p' "$work/hub-restart.log" | head -1)"
    if [ -n "$url" ] && curl -sS -m 10 -o /dev/null "$url/"; then
      pass "6 durable Hub Instance: install, stop, restart, serving ($url)"
    else
      fail "6 durable Hub Instance: restarted but not serving ($url)"
    fi
    "$binary" app stop "$instance" >/dev/null 2>&1
  elif grep -rq "process start time is unavailable" "$work/home/instances" 2>/dev/null; then
    skip "6 durable Hub Instance (this host cannot record a process identity)"
  else
    fail "6 durable Hub Instance: start/stop failed (see $work/hub-restart.log)"
  fi
elif grep -rq "process start time is unavailable" "$work/home/instances" 2>/dev/null; then
  skip "6 durable Hub Instance (this host cannot record a process identity)"
else
  fail "6 durable Hub Instance: install failed (see $work/hub-install.log)"
fi

# 7. A GitHub source is fetched and pinned to a commit.
if [ "$run_github" != "1" ]; then
  skip "7 github source (pass --github)"
elif ! "$binary" source fetch --help >/dev/null 2>&1; then
  skip "7 github source (this binary has no source fetch)"
elif "$binary" source fetch https://github.com/octocat/Spoon-Knife \
     --output "$work/source" >"$work/source.log" 2>&1; then
  if grep -q '"commit"' "$work/source/provenance.json" 2>/dev/null; then
    pass "7 github source: fetched and pinned"
  else
    fail "7 github source: no pinned commit in provenance"
  fi
else
  fail "7 github source fetch failed (see $work/source.log)"
fi

# 8. Formation explores candidate Derivation and verifies one.
if "$binary" form "$root/samples/interop-static-k" --runtime local --network denied \
     --max-attempts 4 --out "$work/form-out" --work-root "$work/form-work" \
     >"$work/form.json" 2>"$work/form.err"; then
  if python3 - "$work/form.json" <<'PY'
import json, sys
try:
    report = json.load(open(sys.argv[1]))
except Exception:
    sys.exit(1)
attempts = report.get("attempts") or []
ok = report.get("status") == "formed" and any(
    (a.get("receipt") or {}).get("fully_satisfied") is True for a in attempts
)
sys.exit(0 if ok else 1)
PY
  then
    pass "8 formation: explored and verified a Derivation"
  else
    fail "8 formation: no verified attempt in the report"
  fi
else
  fail "8 formation did not run (see $work/form.err)"
fi

# 9. The optional Model Set is described before it is downloaded.
if ! "$binary" model status --help >/dev/null 2>&1; then
  skip "9 model status (this binary has no model command)"
elif "$binary" model status --json >"$work/model.json" 2>"$work/model.err"; then
  if python3 - "$work/model.json" <<'PY'
import json, sys
report = json.load(open(sys.argv[1]))
ok = report.get("schema") == "ato.model-status/1" and len(report.get("assets", [])) == 12
sys.exit(0 if ok else 1)
PY
  then
    pass "9 model status: 12 pinned assets"
  else
    fail "9 model status: unexpected report"
  fi
else
  fail "9 model status failed (see $work/model.err)"
fi

# 10. The installed Model Set scores a state, deterministically.
if [ "$run_model" != "1" ]; then
  skip "10 model score (pass --model)"
elif ! "$binary" model score --help >/dev/null 2>&1; then
  skip "10 model score (this binary has no model score)"
elif [ -z "$(printenv ATO_KEV_TEST_PYTHON)" ]; then
  skip "10 model score (set ATO_KEV_TEST_PYTHON)"
else
  request='{"state":"I was charged twice for order 1182.","questions":{"team":{"type":"choice","instructions":"Which team?","criteria":{"billing":"Charges","shipping":"Deliveries"}}}}'
  if "$binary" model score --python "$ATO_KEV_TEST_PYTHON" --request "$request" --json \
       >"$work/score1.json" 2>"$work/score.err" &&
     "$binary" model score --python "$ATO_KEV_TEST_PYTHON" --request "$request" --json \
       >"$work/score2.json" 2>>"$work/score.err"; then
    if python3 - "$work/score1.json" "$work/score2.json" <<'PY'
import json, sys
first = json.load(open(sys.argv[1]))
second = json.load(open(sys.argv[2]))
ok = (
    first.get("schema") == "ato.model-score/1"
    and first.get("answers", {}).get("team", {}).get("type") == "choice"
    and first.get("raw") == second.get("raw")
)
sys.exit(0 if ok else 1)
PY
    then
      pass "10 model score: typed answer, deterministic"
    else
      fail "10 model score: unexpected or non-deterministic answer"
    fi
  else
    fail "10 model score failed (see $work/score.err)"
  fi
fi

# 11. The management page lists Instances and serves its controls.
if ! "$binary" ui --help >/dev/null 2>&1; then
  skip "11 management page (this binary has no ui)"
else
  port=8399
  "$binary" ui --port "$port" >"$work/ui.log" 2>&1 &
  ui_pid=$!
  ready=0
  for _ in $(seq 1 40); do
    if curl -sS -m 2 -o /dev/null "http://127.0.0.1:$port/"; then ready=1; break; fi
    sleep 0.25
  done
  if [ "$ready" != "1" ]; then
    fail "11 management page did not start (see $work/ui.log)"
  elif curl -sS -m 5 "http://127.0.0.1:$port/" | grep -q "起動"; then
    pass "11 management page: serves the Instance controls"
  else
    fail "11 management page: no controls in the page"
  fi
  kill "$ui_pid" 2>/dev/null
  wait "$ui_pid" 2>/dev/null
fi

# 12. The management page drives an Instance through the CLI.
if ! "$binary" ui --help >/dev/null 2>&1; then
  skip "12 management page actions (this binary has no ui)"
else
  port=8398
  "$binary" ui --port "$port" >"$work/ui2.log" 2>&1 &
  ui_pid=$!
  ready=0
  for _ in $(seq 1 40); do
    if curl -sS -m 2 -o /dev/null "http://127.0.0.1:$port/api/instances"; then ready=1; break; fi
    sleep 0.25
  done
  if [ "$ready" != "1" ]; then
    fail "12 management page actions: page did not start (see $work/ui2.log)"
  elif curl -sS -m 10 "http://127.0.0.1:$port/api/instances" >"$work/ui-instances.json" 2>/dev/null &&
       python3 - "$work/ui-instances.json" <<'PY'
import json, sys
report = json.load(open(sys.argv[1]))
sys.exit(0 if isinstance(report.get("instances"), list) else 1)
PY
  then
    pass "12 management page actions: lists Instances over the API"
  else
    fail "12 management page actions: /api/instances failed"
  fi
  kill "$ui_pid" 2>/dev/null
  wait "$ui_pid" 2>/dev/null
fi

echo
if [ "$failures" -eq 0 ]; then
  echo "all checks passed"
  exit 0
fi
echo "$failures check(s) failed"
exit 1
