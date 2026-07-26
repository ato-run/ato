#!/usr/bin/env bash
# Prove — or disprove — that the v1 guest pack is reproducible.
#
# `filesystem.view_digest` is blake3 over the packed ext4 and is committed by the
# Execution Identity, so if the pack is not byte-stable then two builds of one
# program source are two different executions and `capsule.lock` churns on every
# build. The unit tests can only check that the RECIPE asks for determinism
# (`-U`, `-E hash_seed`, `SOURCE_DATE_EPOCH`, `mke2fs -d`, no loop mount); only
# running `mke2fs` twice can show that it delivers it.
#
# This does exactly that: two identical `ato build` runs over one fixture, then
# `sha256sum` on the two images and on the two `execution_id`s.
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
PYEOF
}

run_build one
run_build two

read -r ID_ONE IMG_ONE SRC_ONE < <(tr '\n' ' ' < "$WORK/one.facts")
read -r ID_TWO IMG_TWO SRC_TWO < <(tr '\n' ' ' < "$WORK/two.facts")

echo
echo "== results =="
SHA_ONE="$(sha256sum "$IMG_ONE" | cut -d' ' -f1)"
SHA_TWO="$(sha256sum "$IMG_TWO" | cut -d' ' -f1)"
printf 'source_digest  %s\n               %s\n' "$SRC_ONE" "$SRC_TWO"
printf 'image sha256   %s\n               %s\n' "$SHA_ONE" "$SHA_TWO"
printf 'execution_id   %s\n               %s\n' "$ID_ONE" "$ID_TWO"
echo

[[ "$SRC_ONE" == "$SRC_TWO" ]] || fail "source.digest differs — the projection is not stable, which is a bug ABOVE the pack"

if [[ "$SHA_ONE" != "$SHA_TWO" ]]; then
  echo "The two images differ. What is still varying:" >&2
  # Name the culprit rather than just reporting inequality.
  for img in "$IMG_ONE" "$IMG_TWO"; do
    dumpe2fs -h "$img" 2>/dev/null | grep -Ei 'UUID|Hash Seed|created|write time|mount time|Lifetime' || true
    echo "--"
  done >&2
  fail "the pack is NOT reproducible"
fi

[[ "$ID_ONE" == "$ID_TWO" ]] || fail "images match but execution_id differs — something OTHER than the image is unstable"

echo "PASS: two builds of one source produced byte-identical images and one execution_id"
