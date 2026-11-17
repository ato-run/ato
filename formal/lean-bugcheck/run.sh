#!/usr/bin/env bash
# Lean kernel check and regression tests. RUN_CHECK=1 also refreshes the dead-code inventory.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
mkdir -p "$REPO/.tmp"
OUT="$(mktemp -d "$REPO/.tmp/lean-bugcheck.XXXXXXXX")"
export OUT
export TMPDIR="$OUT"
trap 'rm -rf "$OUT"' EXIT
LEAN="${LEAN:-$HOME/.elan/bin/lean}"
echo '== Lean kernel check'
"$LEAN" "$HERE/AtoBugcheck.lean"
if grep -nE '(^|[^[:alnum:]_])sorry([^[:alnum:]_]|$)' "$HERE/AtoBugcheck.lean"; then
  echo 'Lean source contains sorry' >&2
  exit 1
fi
echo '== missing allowlist root regression'
rustc --edition 2021 "$HERE/repro/path_widening.rs" -o "$OUT/path"
"$OUT/path"
echo '== transfer budget overflow regression'
awk '/^pub struct TransferBudget/,/^}/' "$REPO/services/netd/src/egress/policy.rs" > "$OUT/transfer_budget.rs"
awk '/^impl TransferBudget/,/^}/' "$REPO/services/netd/src/egress/policy.rs" >> "$OUT/transfer_budget.rs"
rustc --edition 2021 -O "$HERE/repro/budget_wrap.rs" -o "$OUT/budget"
"$OUT/budget"
echo '== grant and subprotocol regressions'
(cd "$REPO" && cargo test -p netd --lib surface_websocket_auth::tests)
echo '== manifest error regression'
(cd "$REPO" && cargo test -p ato-materializer-static-web --lib distinguishes_empty_and_invalid_materialization_ids)
if [ "${RUN_CHECK:-0}" = 1 ] && [ -z "${CHECK_JSON:-}" ]; then
  CHECK_JSON="$OUT/check.json"
  echo '== cargo check'
  (cd "$REPO" && cargo check --workspace --all-targets --message-format=json > "$CHECK_JSON")
fi
if [ -n "${CHECK_JSON:-}" ]; then
  echo '== dead code'
  python3 "$HERE/dead-code/scan.py" "$REPO" "$CHECK_JSON" "$HERE/dead-code"
fi
