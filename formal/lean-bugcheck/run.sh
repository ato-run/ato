#!/usr/bin/env bash
# Re-runs everything: Lean kernel check, Rust reproductions, dead-code scan.
#   ./run.sh                    Lean + repros
#   RUN_CHECK=1 ./run.sh        also run `cargo check --workspace --all-targets` and the dead-code scan
#   CHECK_JSON=f.json ./run.sh  dead-code scan from an existing `cargo check --message-format=json` output
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"; REPO="$(cd "$HERE/../.." && pwd)"; OUT="$(mktemp -d)"; export OUT
export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}"
echo "== lean"; (cd "$HERE" && "${LEAN:-$HOME/.elan/bin/lean}" AtoBugcheck.lean 2>&1 | grep -E 'error|sorry' ; echo "lean exit=${PIPESTATUS[0]}")
echo "== repro 1: validate_path widening"
(cd "$HERE/repro" && rustc --edition 2021 path_widening.rs -o "$OUT/path" 2>&1 | grep -E '^error' ; "$OUT/path")
echo "== repro 2: TransferBudget wrap (release)"
awk '/^pub struct TransferBudget/,/^}/' "$REPO/services/netd/src/egress/policy.rs" > "$OUT/transfer_budget.rs"
awk '/^impl TransferBudget/,/^}/' "$REPO/services/netd/src/egress/policy.rs" >> "$OUT/transfer_budget.rs"
(cd "$HERE/repro" && rustc --edition 2021 -O budget_wrap.rs -o "$OUT/budget" 2>&1 | grep -E '^error'; "$OUT/budget")
echo "== repro 3: consumed-grant lockout (temporary in-crate test, reverted afterwards)"
F="$REPO/services/netd/src/surface_websocket_auth.rs"; cp "$F" "$OUT/orig.rs"
python3 - "$F" "$HERE/repro/grant_lockout_test.rs.inc" <<'PY'
import sys; f, inc = sys.argv[1:3]; s = open(f).read().rstrip(); assert s.endswith('}')
open(f, 'w').write(s[:-1].rstrip() + '\n' + open(inc).read())
PY
(cd "$REPO" && cargo test -q -p "$(awk -F'"' '/^name/{print $2; exit}' services/netd/Cargo.toml)" --lib lean_repro 2>&1 | grep -E '^test |test result|^error' )
cp "$OUT/orig.rs" "$F"; git -C "$REPO" diff --quiet -- services/netd && echo "netd source restored"
if [ "${RUN_CHECK:-0}" = 1 ] && [ -z "${CHECK_JSON:-}" ]; then
  CHECK_JSON="$OUT/check.json"; echo "== cargo check (slow)"
  (cd "$REPO" && cargo check --workspace --all-targets --message-format=json > "$CHECK_JSON" 2> "$OUT/check.err") || tail -5 "$OUT/check.err"
fi
if [ -n "${CHECK_JSON:-}" ]; then echo "== dead code"; python3 "$HERE/dead-code/scan.py" "$REPO" "$CHECK_JSON" "$HERE/dead-code"; fi
