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
for model in "$HERE"/*.lean; do
  "$LEAN" "$model"
done
if grep -nE '(^|[^[:alnum:]_])sorry([^[:alnum:]_]|$)' "$HERE"/*.lean; then
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
echo '== discontinuous record counterexample'
# One build for every rlib the repros link, so cargo unifies features once.
(cd "$REPO" && cargo build -p ato-objects -p ato-computation -p ato-kernel -p ato-compose -p ato-local-execution -p ato-portable-application)
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}"
case "$TARGET_DIR" in /*) ;; *) TARGET_DIR="$REPO/$TARGET_DIR" ;; esac
rustc --edition 2024 "$HERE/repro/record_continuity.rs" \
  --extern "ato_objects=$TARGET_DIR/debug/libato_objects.rlib" \
  --extern "ato_computation=$TARGET_DIR/debug/libato_computation.rlib" \
  -L "dependency=$TARGET_DIR/debug/deps" -o "$OUT/record-continuity"
"$OUT/record-continuity"
echo '== record recovery, cursor write and branch origin counterexamples'
for repro in record_recovery record_cursor_write record_branch_origin; do
  rustc --edition 2024 "$HERE/repro/$repro.rs" \
    --extern "ato_objects=$TARGET_DIR/debug/libato_objects.rlib" \
    --extern "ato_computation=$TARGET_DIR/debug/libato_computation.rlib" \
    -L "dependency=$TARGET_DIR/debug/deps" -o "$OUT/$repro"
  OUT="$OUT" "$OUT/$repro"
done
echo '== egress CIDR covers every IPv4-embedded IPv6 form'
awk '/^const DENIED_RANGES/,/^\];/' "$REPO/services/netd/src/egress/gate.rs" > "$OUT/egress_denied_ranges.rs"
IPNET="$(ls -t "$TARGET_DIR"/debug/deps/libipnet-*.rlib | head -1)"
OUT="$OUT" rustc --edition 2024 "$HERE/repro/netd_egress_cidr.rs" \
  --extern "ipnet=$IPNET" -L "dependency=$TARGET_DIR/debug/deps" -o "$OUT/netd-egress-cidr"
OUT="$OUT" "$OUT/netd-egress-cidr"
echo '== hosted lease failure codes, lifetime and attempt journal'
rustc -O --edition 2024 "$HERE/repro/hosted_lease_logic.rs" -o "$OUT/hosted-lease-logic"
"$OUT/hosted-lease-logic"
echo '== duplicate kernel registration and compose depth counterexamples'
rustc --edition 2024 "$HERE/repro/core_boundaries.rs" \
  --extern "ato_objects=$TARGET_DIR/debug/libato_objects.rlib" \
  --extern "ato_computation=$TARGET_DIR/debug/libato_computation.rlib" \
  --extern "ato_kernel=$TARGET_DIR/debug/libato_kernel.rlib" \
  --extern "ato_compose=$TARGET_DIR/debug/libato_compose.rlib" \
  -L "dependency=$TARGET_DIR/debug/deps" -o "$OUT/core-boundaries"
"$OUT/core-boundaries"
echo '== default compose depth budget bypassed through Kernel::enabled'
rustc --edition 2024 "$HERE/repro/kernel_depth_default.rs" \
  --extern "ato_objects=$TARGET_DIR/debug/libato_objects.rlib" \
  --extern "ato_computation=$TARGET_DIR/debug/libato_computation.rlib" \
  --extern "ato_kernel=$TARGET_DIR/debug/libato_kernel.rlib" \
  --extern "ato_compose=$TARGET_DIR/debug/libato_compose.rlib" \
  -L "dependency=$TARGET_DIR/debug/deps" -o "$OUT/kernel-depth-default"
"$OUT/kernel-depth-default"
echo '== authoring seal gate and compose wiring counterexamples'
rustc --edition 2024 "$HERE/repro/semantic_compose_wiring.rs" \
  --extern "ato_objects=$TARGET_DIR/debug/libato_objects.rlib" \
  --extern "ato_computation=$TARGET_DIR/debug/libato_computation.rlib" \
  --extern "ato_compose=$TARGET_DIR/debug/libato_compose.rlib" \
  -L "dependency=$TARGET_DIR/debug/deps" -o "$OUT/semantic-wiring"
"$OUT/semantic-wiring"
ATO_LOCAL_EXECUTION="$(ls -t "$TARGET_DIR"/debug/deps/libato_local_execution-*.rlib | head -1)"
rustc --edition 2024 "$HERE/repro/semantic_authoring_gate.rs" \
  --extern "ato_local_execution=$ATO_LOCAL_EXECUTION" \
  -L "dependency=$TARGET_DIR/debug/deps" -o "$OUT/semantic-authoring"
mkdir -p "$OUT/lean-bugcheck"
(cd "$OUT/lean-bugcheck" && "$OUT/semantic-authoring")
echo '== durable local Instance starting-lease orphaning'
rustc --edition 2024 "$HERE/repro/instance_lease.rs" \
  --extern "ato_portable_application=$TARGET_DIR/debug/libato_portable_application.rlib" \
  -L "dependency=$TARGET_DIR/debug/deps" -o "$OUT/instance-lease"
mkdir -p "$OUT/instance"
OUT="$OUT/instance" "$OUT/instance-lease"
echo '== portable capsule v2/v3/v4 closure, dispatch and bounds'
for repro in bundle_v2_closure bundle_v3_shape bundle_v3_closure; do
  rustc --edition 2024 "$HERE/repro/$repro.rs" \
    --extern "ato_objects=$TARGET_DIR/debug/libato_objects.rlib" \
    --extern "ato_computation=$TARGET_DIR/debug/libato_computation.rlib" \
    --extern "ato_portable_application=$TARGET_DIR/debug/libato_portable_application.rlib" \
    -L "dependency=$TARGET_DIR/debug/deps" -o "$OUT/$repro"
  "$OUT/$repro"
done
echo '== rejected resume overwrites the workspace snapshot'
ATO_CLI="${ATO_CLI:-$TARGET_DIR/debug/ato}"
if [ ! -x "$ATO_CLI" ]; then
  (cd "$REPO" && cargo build -p ato-cli)
fi
ATO_CLI="$ATO_CLI" "$HERE/repro/cli_resume.sh"
echo '== CLI lifecycle ordering, symlink escape and stale lease'
mkdir -p "$OUT/lifecycle"
OUT="$OUT/lifecycle" ATO_CLI="$ATO_CLI" "$HERE/repro/cli_lifecycle_order.sh"
if [ -n "${ATO_API_DIR:-}" ]; then
  echo '== ato-api resident restart after Runner recovery (AlwaysOnRecovery)'
  (cd "$ATO_API_DIR" && ./node_modules/.bin/vitest run src/tests/always-on-policy.test.ts src/tests/runner-lease-state-authz.test.ts)
fi
if [ "${RUN_CHECK:-0}" = 1 ] && [ -z "${CHECK_JSON:-}" ]; then
  CHECK_JSON="$OUT/check.json"
  echo '== cargo check'
  (cd "$REPO" && cargo check --workspace --all-targets --message-format=json > "$CHECK_JSON")
fi
if [ -n "${CHECK_JSON:-}" ]; then
  echo '== dead code'
  python3 "$HERE/dead-code/scan.py" "$REPO" "$CHECK_JSON" "$HERE/dead-code"
fi
