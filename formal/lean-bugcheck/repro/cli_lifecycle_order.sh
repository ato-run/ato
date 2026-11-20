#!/usr/bin/env bash
# CLI lifecycle ordering: a failing `ato resume` restores the workspace before it
# validates, so a rejected Run still overwrites the working tree.
# Isolated: every project lives under $OUT, ATO_HOME is $OUT/ato-home.
# Usage: OUT=<dir> ATO_CLI=<target/debug/ato> repro/cli_lifecycle_order.sh
set -euo pipefail
: "${OUT:?OUT is required}" "${ATO_CLI:?ATO_CLI is required}"

export ATO_HOME="$OUT/ato-home"
mkdir -p "$ATO_HOME"

fail() { echo "FAIL: $*" >&2; exit 1; }

# write_config <project>: the same shape as repro/cli_resume.sh (one process
# Adapter, one workspace Adapter).
write_config() {
    cat > "$1/capsule.toml" <<'CONFIG'
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
}

# new_project <project>: config + state.txt + `init --initial-only`.
new_project() {
    local project="$1"
mkdir -p "$project"
    write_config "$project"
    printf 'snapshot' > "$project/state.txt"
    "$ATO_CLI" init "$project" --initial-only > "$OUT/init.log" 2>&1
}

# 1. --branch names an existing branch. Everything is validated *after* the restore.
p1="$OUT/p1"; new_project "$p1"
head_before=$(cat "$p1/.capsule/refs/heads/main")
printf 'unsaved change' > "$p1/state.txt"          # modified since the snapshot
printf 'brand new' > "$p1/newfile.txt"             # never captured
if "$ATO_CLI" resume "$p1" --branch main > "$OUT/p1-resume.log" 2>&1; then
    fail "expected duplicate-branch rejection"
fi
grep -q "already exists" "$OUT/p1-resume.log" || fail "unexpected p1 error: $(cat "$OUT/p1-resume.log")"
[ "$(cat "$p1/state.txt")" = "snapshot" ] || fail "p1 state.txt was not overwritten"
[ "$(cat "$p1/newfile.txt")" = "brand new" ] || fail "p1 newfile.txt should survive (restore is a union)"
[ "$(cat "$p1/.capsule/refs/heads/main")" = "$head_before" ] || fail "p1 refs changed"
echo 'BUG 1: rejected --branch <existing> overwrote the modified project file'

# 2. --branch names an invalid branch (a slash is a normal-looking branch name). The
#    failure comes from create_branch's name validation, after the restore.
p2="$OUT/p2"; new_project "$p2"
printf 'unsaved change' > "$p2/state.txt"
if "$ATO_CLI" resume "$p2" --branch "feature/x" > "$OUT/p2-resume.log" 2>&1; then
    fail "expected invalid-branch-name rejection"
fi
[ "$(cat "$p2/state.txt")" = "snapshot" ] || fail "p2 state.txt was not overwritten"
echo 'BUG 2: rejected --branch feature/x (invalid name) overwrote the modified project file'

# 3. The durable Run cannot start here (macOS sandbox denies ps, so the worker exits
#    before ACTIVE). Any start failure happens after the restore.
p3="$OUT/p3"; new_project "$p3"
printf 'unsaved change' > "$p3/state.txt"
if "$ATO_CLI" resume "$p3" > "$OUT/p3-resume.log" 2>&1; then
    echo 'NOTE: p3 Run started; skipping the start-failure case'
else
    [ "$(cat "$p3/state.txt")" = "snapshot" ] || fail "p3 state.txt was not overwritten"
    [ -e "$p3/.capsule/runs/active.json" ] && fail "p3 left an active Run lease"
    echo 'BUG 3: a Run that never became ACTIVE still overwrote the project file'
fi

# 4. Stale lease left by a crashed Run (constructed fixture in the exact
#    ActiveRunWire shape; a crash leaves this file because nothing collects it).
p4="$OUT/p4"; new_project "$p4"
head=$(cat "$p4/.capsule/refs/heads/main")
printf 'unsaved change' > "$p4/state.txt"
cat > "$p4/.capsule/runs/active.json" <<LEASE
{"token":"1-0-0","branch":"main","branch_base":"$head","head":"$head","record_seq":0,"pid":0,"process_start_time":"","process_group":0,"boot_session":"","status":"active"}
LEASE
if "$ATO_CLI" resume "$p4" > "$OUT/p4-resume.log" 2>&1; then
    fail "expected stale-lease rejection"
fi
[ "$(cat "$p4/state.txt")" = "snapshot" ] || fail "p4 state.txt was not overwritten"
echo "BUG 4: resume with a stale active Run was refused with: $(cat "$OUT/p4-resume.log")"
if "$ATO_CLI" stop "$p4" > "$OUT/p4-stop.log" 2>&1; then
    fail "expected the stale lease to make stop fail"
fi
echo "BUG 4b: stop on the same stale lease: $(cat "$OUT/p4-stop.log")"

# 5. restore_workspace follows a symlink inside the project: checked_path validates
#    only the relative string, so a symlinked directory redirects the write outside.
p5="$OUT/p5"; outside="$OUT/outside"
mkdir -p "$outside" "$p5/sub"
write_config "$p5"
printf 'inside' > "$p5/sub/data.txt"
"$ATO_CLI" init "$p5" --initial-only > "$OUT/p5-init.log" 2>&1
rm -rf "$p5/sub"; ln -s "$outside" "$p5/sub"
"$ATO_CLI" resume "$p5" > "$OUT/p5-resume.log" 2>&1 || true
[ "$(cat "$outside/data.txt" 2>/dev/null)" = "inside" ] || fail "p5 symlink escape did not reproduce"
echo 'BUG 5: restore wrote snapshot content outside the project through a symlinked directory'

echo 'DONE: every rejected resume above changed the workspace.'
