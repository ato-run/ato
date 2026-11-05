#!/usr/bin/env bash
# Verify that two different source trees satisfy one Contract.
# Requires the ato CLI on PATH (cargo install --git ... ato-cli).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
out="$root/.tmp/contract-preserving"
mkdir -p "$out"

ato run "$here/a.capsule" --no-open --verification-receipt "$out/a.json" >/dev/null
ato run "$here/b.capsule" --no-open --verification-receipt "$out/b.json" >/dev/null

python3 - "$out/a.json" "$out/b.json" <<'PY'
import json, sys
a = json.load(open(sys.argv[1]))
b = json.load(open(sys.argv[2]))
print("A  contract_ref:", a["contract_ref"])
print("A derivation_ref:", a["derivation_ref"])
print("B  contract_ref:", b["contract_ref"])
print("B derivation_ref:", b["derivation_ref"])
print("A fully_satisfied:", a["fully_satisfied"])
print("B fully_satisfied:", b["fully_satisfied"])
ok = (
    a["fully_satisfied"]
    and b["fully_satisfied"]
    and a["contract_ref"] == b["contract_ref"]
    and a["derivation_ref"] != b["derivation_ref"]
)
print("PASS: same K, different derivations" if ok else "FAIL")
raise SystemExit(0 if ok else 1)
PY
