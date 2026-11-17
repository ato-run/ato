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
#   WORK=…                 default: a fresh .tmp/solidcraft-verify-<time>-<pid>
set -euo pipefail

SAMPLE_DIR="${SAMPLE_DIR:-samples/solidcraft}"
ATO="${ATO:-ato}"
WORK="${WORK:-.tmp/solidcraft-verify-$(date -u +%Y%m%dT%H%M%SZ)-$$}"
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

# D2: explicitly approve the managed build and the locally built artifact.
# D1 retains require-valid; only D2 declares the local signature override.
D2="$WORK/d2-receipt.json"
"$ATO" run "$BUNDLE" --derivation "$SOURCE" --no-open --allow-native-source-build --allow-unsigned-native \
  --verification-receipt "$D2" >"$WORK/d2.log" 2>&1 || {
  cat "$WORK/d2.log" >&2
  fail "the source-built Derivation did not run"
}
pass "source-built Derivation ran"

# A second run of the same source Derivation must reuse the content-hash cache
# instead of building again.
D2B="$WORK/d2-second-receipt.json"
"$ATO" run "$BUNDLE" --derivation "$SOURCE" --no-open --allow-native-source-build --allow-unsigned-native \
  --verification-receipt "$D2B" >"$WORK/d2-second.log" 2>&1 || {
  cat "$WORK/d2-second.log" >&2
  fail "the second source-built Run did not run"
}
pass "source-built Derivation ran again"

python3 - "$D1" "$D2" "$D2B" "$CONTRACT" "$PREBUILT" "$SOURCE" "$BUNDLE" "$ATO_HOME" "$WORK" <<'PY' || exit 1
import hashlib, json, re, sys
from pathlib import Path

def load(path):
    return json.loads(Path(path).read_text())

def digest(path):
    hasher = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            hasher.update(chunk)
    return 'sha256:' + hasher.hexdigest()

receipt_paths = sys.argv[1:4]
d1, d2, d2b = receipts = [load(p) for p in receipt_paths]
contract, prebuilt, source = sys.argv[4:7]
bundle, home, work = map(Path, sys.argv[7:10])
problems = []
expected_bundle = digest(bundle)
attempts = []
for name, receipt, route in zip(('D1', 'D2', 'D2 second'), receipts, (prebuilt, source, source)):
    if receipt.get('schema') != 'ato.contract-verification-receipt/5':
        problems.append(f'{name}: unexpected receipt schema')
    if receipt.get('contract_ref') != contract:
        problems.append(f'{name}: contract_ref differs from the packed Contract')
    if receipt.get('derivation_ref') != route:
        problems.append(f'{name}: receipt does not name the selected Derivation')
    if receipt.get('bundle_sha256') != expected_bundle:
        problems.append(f'{name}: receipt does not name the same bundle bytes')
    if receipt.get('fully_satisfied') is not True:
        problems.append(f'{name}: fully_satisfied is not true')
    observations = receipt.get('observations') or []
    if not observations:
        problems.append(f'{name}: no observations')
    for observation in observations:
        evidence = observation.get('evidence') or {}
        if observation.get('outcome') != 'satisfied':
            problems.append(f'{name}: an observation is not satisfied')
        if not evidence.get('path') or not re.fullmatch(r'sha256:[0-9a-f]{64}', evidence.get('body_sha256', '')) or evidence.get('captured_bytes', 0) <= 0:
            problems.append(f'{name}: missing or invalid observation evidence')
    execution = receipt.get('execution') or {}
    attempts.append(execution.get('attempt_id'))
    native = execution.get('native') or {}
    expected_policy = 'require-valid' if name == 'D1' else 'require-valid-or-local-override'
    if native.get('signature_policy') != expected_policy or native.get('local_override') is not (name != 'D1'):
        problems.append(f'{name}: the signature decision does not match the explicit local approval')
if prebuilt == source:
    problems.append('the selected Derivations must differ')
if any(not attempt for attempt in attempts) or len(set(attempts)) != 3:
    problems.append('each receipt must belong to a distinct non-empty attempt')
if problems:
    raise SystemExit('\n'.join('FAIL: ' + problem for problem in problems))

# Receipt /5 does not contain build_cache. Inspect this attempt's retained
# build staging area and the verified cache manifest instead of treating a
# second successful launch (or its elapsed time) as proof of cache reuse.
execution = d2b['execution']
native = execution['native']
entry = Path(native['entry'])
if entry.is_absolute() or '..' in entry.parts or entry.parts[:3] != ('.ato-build', 'target', 'release'):
    raise SystemExit('FAIL: unexpected source entry')
launched = Path(execution['runtime_executable']).resolve(strict=True)
launched.relative_to(home.resolve() / 'cache')
suffix = ('native-build', 'staging', *entry.parts)
if launched.parts[-len(suffix):] != suffix:
    raise SystemExit('FAIL: source receipt does not point to the build staging area')
build_root = launched.parents[len(entry.parts)]
if (build_root / 'source').exists() or (build_root / 'scratch' / 'build.sb').exists():
    raise SystemExit('FAIL: the second source Run built again instead of reusing the cache')
artifact_digest = digest(launched)
if artifact_digest != native['artifact_ref'] or artifact_digest != d2['execution']['native']['artifact_ref']:
    raise SystemExit('FAIL: source Runs did not launch the same verified artifact')
matching = []
for manifest in (home / 'cache' / 'native-build').glob('*/manifest.txt'):
    lines = manifest.read_text().splitlines()
    if len(lines) == 4 and lines[0] == 'sha256:' + manifest.parent.name and lines[1] == artifact_digest:
        cached = manifest.parent / entry
        if cached.is_file() and digest(cached) == artifact_digest:
            matching.append((manifest, lines))
if len(matching) != 1:
    raise SystemExit('FAIL: no unique matching verified build-cache entry')
manifest, lines = matching[0]
evidence = {
    'schema': 'ato.solidcraft-cache-acceptance/1',
    'receipt_sha256': digest(receipt_paths[2]),
    'attempt_id': attempts[2],
    'contract_ref': contract,
    'derivation_ref': source,
    'build_cache': 'hit',
    'cache_key': lines[0],
    'artifact_sha256': artifact_digest,
    'toolchain': lines[2],
    'build_argv': lines[3],
    'cache_manifest': str(manifest.resolve()),
    'staged_entry': str(launched),
    'source_tree_created': False,
    'build_profile_created': False,
}
(work / 'cache-evidence.json').write_text(json.dumps(evidence, indent=2) + '\n')
print('contract_ref:', contract)
print('D1 derivation_ref:', prebuilt)
print('D2 derivation_ref:', source)
print('D1 / D2 / D2 second fully_satisfied: true')
print('D2 second build_cache: hit (verified artifact, cache manifest, and no build in this attempt)')
PY

printf '\nSolidCraft two-Derivation verification passed.\n'
printf 'receipts: %s %s %s\n' "$D1" "$D2" "$D2B"
