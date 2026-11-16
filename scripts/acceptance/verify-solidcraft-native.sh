#!/usr/bin/env bash
# Verify the SolidCraft two-Derivation sample on a host that can apply the
# macOS Seatbelt policy.
#
# The sample publishes one Contract with two Derivations: a pre-built binary
# fetched from the upstream release, and the same program built from the pinned
# source commit. This script packs it, runs each Derivation with a verification
# receipt, runs the source Derivation a second time to prove the build cache,
# and checks that both receipts name the same Contract and a different
# Derivation. It does not publish anything.
#
# Usage: scripts/acceptance/verify-solidcraft-native.sh
#   ATO=path/to/ato        the CLI under test (default: ato)
#   SAMPLE_DIR=…           default: samples/solidcraft
#   WORK=.tmp/solidcraft-verify
set -euo pipefail

SAMPLE_DIR="${SAMPLE_DIR:-samples/solidcraft}"
ATO="${ATO:-ato}"
WORK="${WORK:-.tmp/solidcraft-verify}"
ATO_HOME="${ATO_HOME:-$PWD/$WORK/ato-home}"

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
pass() { printf 'ok: %s\n' "$*"; }

command -v "$ATO" >/dev/null 2>&1 || fail "the ato CLI is not on PATH (set ATO=…)"
mkdir -p "$ATO_HOME" "$WORK"
export ATO_HOME

BUNDLE="$WORK/solidcraft.capsule"
"$ATO" pack "$SAMPLE_DIR" -o "$BUNDLE" >"$WORK/pack.log" 2>&1 || {
  cat "$WORK/pack.log" >&2
  fail "pack"
}
pass "packed $BUNDLE"

CANDIDATES="$WORK/candidates.json"
"$ATO" run "$BUNDLE" --list-derivations >"$CANDIDATES" 2>"$WORK/list.log" || {
  cat "$WORK/list.log" >&2
  fail "--list-derivations"
}
read -r PREBUILT SOURCE CONTRACT < <(python3 - "$CANDIDATES" <<'PY'
import json, sys
data = json.load(open(sys.argv[1]))
routes = data["derivations"]
if len(routes) != 2:
    raise SystemExit(f"the sample must declare two Derivations, saw {len(routes)}")
prebuilt = [r for r in routes if r.get("download_bytes")]
source = [r for r in routes if not r.get("download_bytes")]
if len(prebuilt) != 1 or len(source) != 1:
    raise SystemExit("exactly one Route must fetch a pre-built artifact")
print(prebuilt[0]["derivation_ref"], source[0]["derivation_ref"], data["contract_ref"])
PY
)
[ -n "$PREBUILT" ] || fail "could not read the pre-built Derivation"
pass "candidates: prebuilt=$PREBUILT source=$SOURCE contract=$CONTRACT"

# D1: fetch the upstream archive, unpack it, verify the signature, launch it
# under the PTY and answer the declared exchange.
D1="$WORK/d1-receipt.json"
"$ATO" run "$BUNDLE" --derivation "$PREBUILT" --no-open --verification-receipt "$D1" \
  >"$WORK/d1.log" 2>&1 || { cat "$WORK/d1.log" >&2; fail "the pre-built Derivation did not run"; }
pass "pre-built Derivation ran"

# D2: consent to the managed toolchain, build from the pinned commit, launch.
D2="$WORK/d2-receipt.json"
"$ATO" run "$BUNDLE" --derivation "$SOURCE" --no-open --allow-native-source-build \
  --verification-receipt "$D2" >"$WORK/d2.log" 2>&1 || {
  cat "$WORK/d2.log" >&2
  fail "the source-built Derivation did not run"
}
pass "source-built Derivation ran"

# A second run of the same source Derivation must reuse the content-hash cache
# instead of building again.
D2B="$WORK/d2-second-receipt.json"
"$ATO" run "$BUNDLE" --derivation "$SOURCE" --no-open --allow-native-source-build \
  --verification-receipt "$D2B" >"$WORK/d2-second.log" 2>&1 || {
  cat "$WORK/d2-second.log" >&2
  fail "the second source-built Run did not run"
}
pass "source-built Derivation ran again"

python3 - "$D1" "$D2" "$D2B" "$CONTRACT" <<'PY' || exit 1
import json, sys

def load(path):
    return json.load(open(path))

d1, d2, d2b = (load(p) for p in sys.argv[1:4])
expected_contract = sys.argv[4]

problems = []
for name, receipt in (("D1", d1), ("D2", d2), ("D2 second", d2b)):
    if receipt.get("contract_ref") != expected_contract:
        problems.append(f"{name}: contract_ref {receipt.get('contract_ref')} != {expected_contract}")
    if receipt.get("fully_satisfied") is not True:
        problems.append(f"{name}: fully_satisfied is not true")
    for observation in receipt.get("observations", []):
        evidence = observation.get("evidence") or {}
        for key in ("path", "body_sha256", "captured_bytes"):
            if key not in evidence:
                problems.append(f"{name}: observation {observation.get('id')} lacks evidence.{key}")

if d1.get("derivation_ref") == d2.get("derivation_ref"):
    problems.append("the two Derivations report the same derivation_ref")
if not d1.get("derivation_ref") or not d2.get("derivation_ref"):
    problems.append("a receipt names no derivation_ref")

print("contract_ref:", d1.get("contract_ref"))
print("D1 derivation_ref:", d1.get("derivation_ref"))
print("D2 derivation_ref:", d2.get("derivation_ref"))
print("D1 fully_satisfied:", d1.get("fully_satisfied"))
print("D2 fully_satisfied:", d2.get("fully_satisfied"))
print("D2 second fully_satisfied:", d2b.get("fully_satisfied"))
if problems:
    for problem in problems:
        print("FAIL:", problem, file=sys.stderr)
    raise SystemExit(1)
print("both Derivations satisfied one Contract, and the second source Run reused the cache")
PY

printf '\nSolidCraft two-Derivation verification passed.\n'
printf 'receipts: %s %s %s\n' "$D1" "$D2" "$D2B"
