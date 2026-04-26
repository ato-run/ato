#!/usr/bin/env bash
# Sync docs/upstream-issues/*.md to ato-run/ato-cli as GitHub Issues.
# Idempotent: skips issues whose title already exists upstream.
# Requires: gh auth login (run `gh auth login` first)
#
# Usage: bash tools/sync-upstream-issues.sh [--dry-run]
set -euo pipefail

REPO="ato-run/ato-cli"
ISSUES_DIR="$(cd "$(dirname "$0")/../docs/upstream-issues" && pwd)"
DRY_RUN="${1:-}"

if ! gh auth status &>/dev/null; then
  echo "ERROR: gh is not authenticated. Run: gh auth login"
  exit 1
fi

filed=0; skipped=0

for f in "$ISSUES_DIR"/[0-9]*.md; do
  [ -f "$f" ] || continue

  title=$(grep -m1 '^# ' "$f" | sed 's/^# //')
  if [ -z "$title" ]; then
    echo "warn: $f has no H1 title — skipping"
    continue
  fi

  # Extract labels from HTML comment block (<!-- LABELS: ... -->)
  labels=$(grep -m1 'LABELS:' "$f" | sed 's/.*LABELS: *//' | tr -d ' */' | tr ',' '\n' | \
           xargs -I{} echo "--label {}" | tr '\n' ' ')

  existing=$(gh issue list --repo "$REPO" \
    --search "\"$title\" in:title" \
    --json number --jq 'length')

  if [ "$existing" -gt 0 ]; then
    echo "skip: \"$title\" (already exists upstream)"
    ((skipped++)) || true
    continue
  fi

  body=$(awk '/^<!--/{skip=1} skip && /-->/{skip=0; next} !skip' "$f" | tail -n +3)

  if [ "$DRY_RUN" = "--dry-run" ]; then
    echo "[dry-run] would create: \"$title\""
  else
    gh issue create --repo "$REPO" \
      --title "$title" \
      --body "$body" \
      $labels
    echo "created: \"$title\""
    ((filed++)) || true
  fi
done

echo "---"
echo "filed: $filed, skipped: $skipped"
