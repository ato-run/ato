#!/usr/bin/env bash
# Exercise only an isolated project under OUT; never touch an existing project.
set -euo pipefail
: "${OUT:?OUT is required}" "${ATO_CLI:?ATO_CLI is required}"
PROJECT="$OUT/cli-project"
mkdir -p "$PROJECT" "$OUT/ato-home"
export ATO_HOME="$OUT/ato-home"
cat > "$PROJECT/capsule.toml" <<'CONFIG'
schema = 1
[[process]]
id = "app"
command = ["sh", "-c", "true"]
cwd = "."
[[adapter]]
target = "app"
use = "ato.process@1"
[[adapter]]
target = "workspace"
use = "ato.workspace@1"
[encap]
materializers = ["ato.replay@1"]
CONFIG
printf 'snapshot' > "$PROJECT/state.txt"
"$ATO_CLI" init "$PROJECT" --initial-only > "$OUT/init.log" 2>&1
printf 'unsaved change' > "$PROJECT/state.txt"
if "$ATO_CLI" resume "$PROJECT" --branch main > "$OUT/resume.log" 2>&1; then
    echo 'expected duplicate branch rejection' >&2
    exit 1
fi
grep -q 'already exists' "$OUT/resume.log"
[ "$(cat "$PROJECT/state.txt")" = 'snapshot' ]
echo 'rejected resume overwrote unsaved project file'
