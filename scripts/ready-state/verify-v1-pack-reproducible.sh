#!/usr/bin/env bash
# Prove — or disprove — that two builds of one program source are one execution.
#
# `execution_id` is what `capsule.lock` publishes and what the wizard lane
# enqueues against, so if it moves between two identical builds then a rebuild
# is a different capsule, the lock churns, and two builder hosts never agree.
# The unit tests cannot show this: it is a property of the whole lane running
# twice against a real docker and a real mke2fs.
#
# This runs it. Two identical `ato build` runs over one fixture, then compares
# `source_digest` and `execution_id`.
#
# The packed ext4 is NOT expected to be byte-identical, and that is deliberate:
# `mke2fs` stamps every inode with the wall clock and ignores SOURCE_DATE_EPOCH
# (measured, e2fsprogs 1.47.0). The identity commits the guest's CONTENTS —
# `snapshot::guest_filesystem_digest` — precisely so it does not depend on how
# the allocator laid them out or on which e2fsprogs the builder has installed.
# The image hash is reported below as information, never as a gate.
#
# Requires: Linux, root (docker export needs it to restore ownership), docker,
# e2fsprogs >= 1.45.7 (SOURCE_DATE_EPOCH support). Prints the versions it found
# so a failure can be told apart from an unsupported toolchain.
#
#   sudo ./scripts/ready-state/verify-v1-pack-reproducible.sh [/path/to/ato]
set -euo pipefail

ATO_BIN="${1:-$(command -v ato || true)}"
if [[ -z "$ATO_BIN" || ! -x "$ATO_BIN" ]]; then
  echo "usage: $0 /path/to/ato    (or put ato on PATH)" >&2
  exit 2
fi
# Absolute, because each build runs in its own directory: a relative
# `target/debug/ato` resolves against the repo root here and against the fixture
# there, where it does not exist. A bare name goes through PATH rather than
# being joined onto $PWD, which would invent a path that does not exist.
if [[ "$ATO_BIN" == */* ]]; then
  ATO_BIN="$(cd "$(dirname "$ATO_BIN")" && pwd)/$(basename "$ATO_BIN")"
else
  ATO_BIN="$(command -v "$ATO_BIN")"
fi
[[ -x "$ATO_BIN" ]] || { echo "not executable: $ATO_BIN" >&2; exit 2; }
echo "ato binary: $ATO_BIN"

fail() { echo "FAIL: $*" >&2; exit 1; }

[[ "$(uname -s)" == "Linux" ]] || fail "this must run on Linux; mke2fs is the thing under test"
[[ "$(id -u)" == "0" ]] || fail "run as root: 'docker export | tar -x' needs it to restore ownership"
command -v docker >/dev/null || fail "docker not found"

echo "== toolchain =="
docker --version
mke2fs -V 2>&1 | head -1
"$ATO_BIN" --version || true

# e2fsprogs honours SOURCE_DATE_EPOCH from 1.45.7. Below that the superblock
# timestamps are wall-clock and this WILL fail — report it as unsupported rather
# than as a regression in the pack.
E2FS_VERSION="$(mke2fs -V 2>&1 | head -1 | sed -n 's/.*mke2fs \([0-9][0-9.]*\).*/\1/p')"
echo "e2fsprogs: ${E2FS_VERSION:-unknown}"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# A minimal Step-4-subset capsule. Deliberately NOT a git checkout: a v1 build
# refuses a working tree, because a root-level .git is neither a withholdable
# control file nor reproducible bytes.
FIXTURE="$WORK/fixture"
mkdir -p "$FIXTURE"
cat > "$FIXTURE/capsule.toml" <<'TOML'
schema_version = "1"
name = "repro-fixture"
version = "0.1.0"

[run]
command = ["python3", "app.py"]

[web]
port = 8080
bind = "0.0.0.0"

[seal_at]
command = ["curl", "-fsS", "http://127.0.0.1:8080/"]
TOML
cat > "$FIXTURE/app.py" <<'PY'
import http.server, socketserver
class H(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200); self.end_headers(); self.wfile.write(b"ok")
socketserver.TCPServer(("0.0.0.0", 8080), H).serve_forever()
PY

run_build() {
  local label="$1"
  echo "== build $label =="
  # A fresh copy per run so neither build sees the other's capsule.lock as a
  # pre-existing control file — the withheld set is identity-bearing.
  local dir="$WORK/$label"
  cp -a "$FIXTURE" "$dir"
  # `--json` is a GLOBAL flag and prints the result pretty at the end, after
  # whatever the reporter wrote — so the payload is the last balanced object in
  # the stream, not a line.
  ( cd "$dir" && "$ATO_BIN" --json build . ) > "$WORK/$label.out" 2>"$WORK/$label.err" \
    || { echo "--- $label stdout ---"; cat "$WORK/$label.out"; \
         echo "--- $label stderr ---"; cat "$WORK/$label.err"; fail "ato build ($label) failed"; }
  python3 - "$WORK/$label.out" <<'PYEOF' > "$WORK/$label.facts"
import json, sys

text = open(sys.argv[1]).read()
decoder = json.JSONDecoder()
found = None
for start in range(len(text)):
    if text[start] != "{":
        continue
    try:
        value, _ = decoder.raw_decode(text[start:])
    except ValueError:
        continue
    if isinstance(value, dict) and value.get("v1"):
        found = value["v1"]          # keep scanning: the LAST one is the result
if not found:
    sys.exit("no v1 build result in --json output (was this a v0.3 manifest?)")
print(found["execution_id"])
print(found["guest_image"])
print(found["source_digest"])
print(found["filesystem_view_digest"])
print(found["guest_image_digest"])
PYEOF
}

run_build one
run_build two

# Line-addressed rather than `read`-from-a-pipe: `read` returns non-zero when it
# hits EOF without a trailing delimiter, which under `set -e` kills the script
# silently — after both builds have already succeeded, so the log shows the work
# done and no reason for the failure.
facts() { sed -n "${2}p" "$WORK/$1.facts"; }
ID_ONE="$(facts one 1)";  IMG_ONE="$(facts one 2)";  SRC_ONE="$(facts one 3)"
VIEW_ONE="$(facts one 4)"; ART_ONE="$(facts one 5)"
ID_TWO="$(facts two 1)";  IMG_TWO="$(facts two 2)";  SRC_TWO="$(facts two 3)"
VIEW_TWO="$(facts two 4)"; ART_TWO="$(facts two 5)"
for value in "$ID_ONE" "$IMG_ONE" "$SRC_ONE" "$VIEW_ONE" "$ART_ONE" \
             "$ID_TWO" "$IMG_TWO" "$SRC_TWO" "$VIEW_TWO" "$ART_TWO"; do
  [[ -n "$value" ]] || fail "a build did not report one of its digests"
done
[[ -f "$IMG_ONE" && -f "$IMG_TWO" ]] || fail "a reported guest image does not exist"

echo
echo "== results =="
SHA_ONE="$(sha256sum "$IMG_ONE" | cut -d' ' -f1)"
SHA_TWO="$(sha256sum "$IMG_TWO" | cut -d' ' -f1)"
echo "IDENTITY (must match):"
printf '  source_digest         %s\n                        %s\n' "$SRC_ONE" "$SRC_TWO"
printf '  filesystem.view_digest %s\n                        %s\n' "$VIEW_ONE" "$VIEW_TWO"
printf '  execution_id          %s\n                        %s\n' "$ID_ONE" "$ID_TWO"
echo "MATERIALIZATION (recorded, may differ):"
printf '  artifact digest       %s\n                        %s\n' "$ART_ONE" "$ART_TWO"
printf '  image sha256          %s\n                        %s\n' "$SHA_ONE" "$SHA_TWO"
echo

[[ "$SRC_ONE" == "$SRC_TWO" ]] || fail "source.digest differs — the projection is not stable, which is a bug ABOVE the pack"
[[ "$VIEW_ONE" == "$VIEW_TWO" ]] || fail "filesystem.view_digest differs — the guest CONTENTS are not stable, which is a real difference and not a serialization one"
# The artifact digest is a receipt, so it is checked for PRESENCE, never for
# equality: two packs of one guest filesystem legitimately differ here.
[[ "$ART_ONE" != "$ART_TWO" || "$ART_ONE" == "$ART_TWO" ]]

if [[ "$SHA_ONE" == "$SHA_TWO" ]]; then
  echo "(the packed images also happened to match byte for byte)"
else
  echo "(the packed images differ, as expected: mke2fs stamps inodes with the"
  echo " clock. The identity commits contents, not this serialization.)"
fi

if [[ "$ID_ONE" != "$ID_TWO" ]]; then
  {
    echo "Two builds of one source minted two executions. Everything below is"
    echo "what is still varying; anything NOT listed is already stable."
    echo
    # The full metadata, diffed — superblock AND group descriptors, so a
    # difference that is not a superblock field still gets named. One run of
    # this should be enough to know what to fix next.
    echo "--- dumpe2fs diff ---"
    diff <(dumpe2fs "$IMG_ONE" 2>/dev/null) <(dumpe2fs "$IMG_TWO" 2>/dev/null) || true
    echo
    # And where in the file, in case the difference is in data rather than
    # metadata: the first few differing offsets locate it.
    echo "--- first differing bytes (offset, then each file's octal byte) ---"
    cmp -l "$IMG_ONE" "$IMG_TWO" 2>/dev/null | head -20 || true
    echo "differing byte count: $(cmp -l "$IMG_ONE" "$IMG_TWO" 2>/dev/null | wc -l)"
    echo "(byte differences are expected; they are shown only to locate a"
    echo " CONTENT difference, which would be a real one)"
    echo
    # A byte offset says WHERE but not WHICH FIELD. Stat the same inodes out of
    # both images and diff: that names the field, and an inode timestamp reads
    # very differently from a superblock one.
    echo "--- inode diff (root, and a file the capsule ships) ---"
    for target in "<2>" "/app/app.py" "/sbin/init"; do
      echo "### $target"
      diff <(debugfs -R "stat $target" "$IMG_ONE" 2>/dev/null) \
           <(debugfs -R "stat $target" "$IMG_TWO" 2>/dev/null) || true
    done
  } >&2
  fail "the same program source minted two different execution_ids"
fi

echo "PASS: two builds of one program source minted one execution_id"
