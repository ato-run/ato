#!/usr/bin/env bash
# tools/dogfood-runner.sh
# Sequentially run every sample at L2-functional and report pass/fail.
# Not a CI substitute — for pre-release manual verification.
#
# Usage:
#   ./tools/dogfood-runner.sh [--tier TIER] [--layer LAYER] [--output FILE]
#
# Options:
#   --tier    Only run samples under this tier (e.g. 01-capabilities)
#   --layer   run-sample-checks layer to use (default: L2-functional)
#   --output  Write markdown summary to FILE (default: stdout only)
#
# Exit codes:
#   0  All samples passed (or failed as expected)
#   1  One or more unexpected failures
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LAYER="L2-functional"
TIER_FILTER=""
OUTPUT_FILE=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tier)   TIER_FILTER="$2"; shift 2 ;;
    --layer)  LAYER="$2"; shift 2 ;;
    --output) OUTPUT_FILE="$2"; shift 2 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

pass=0; fail=0; skip=0
declare -a results

check_skip() {
  local health="$1"
  # Skip samples that require desktop interaction
  if node -e "
    const t = require('fs').readFileSync('$health', 'utf8');
    const {parse} = require('smol-toml');
    const h = parse(t);
    process.exit((h.requirements?.requires_desktop || h.requirements?.requires_tty) ? 1 : 0);
  " 2>/dev/null; then
    return 1  # do not skip
  fi
  return 0    # skip
}

for dir in "$REPO_ROOT"/[0-9][0-9]-*/*/ ; do
  [[ -f "$dir/capsule.toml" ]] || continue
  [[ -f "$dir/health.toml" ]] || continue

  rel="${dir#"$REPO_ROOT/"}"
  rel="${rel%/}"

  # Apply tier filter
  if [[ -n "$TIER_FILTER" && "$rel" != "$TIER_FILTER"/* ]]; then
    continue
  fi

  # Skip samples requiring desktop/tty
  if check_skip "$dir/health.toml"; then
    ((skip++))
    results+=("⏭  $rel (skipped: requires_desktop or requires_tty)")
    continue
  fi

  echo "=== $rel ==="
  if node "$REPO_ROOT/tools/run-sample-checks.mjs" "$dir" --layer "$LAYER" 2>&1; then
    ((pass++))
    results+=("✅ $rel")
  else
    ((fail++))
    results+=("❌ $rel")
  fi
  echo ""
done

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
printf '%s\n' "${results[@]}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "pass: $pass  fail: $fail  skip: $skip"

if [[ -n "$OUTPUT_FILE" ]]; then
  {
    echo "# Dogfood Run — $(date -u +%Y-%m-%dT%H:%MZ)"
    echo ""
    echo "Layer: \`$LAYER\`"
    echo ""
    echo "| Result | Sample |"
    echo "|--------|--------|"
    for r in "${results[@]}"; do
      echo "| ${r:0:2} | \`${r:3}\` |"
    done
    echo ""
    echo "**pass: $pass  fail: $fail  skip: $skip**"
  } > "$OUTPUT_FILE"
  echo "report written to $OUTPUT_FILE"
fi

[[ "$fail" -eq 0 ]]
