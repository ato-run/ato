#!/usr/bin/env bash
# ADR-015 step 6: the v1 Execution Identity, end to end, on a KVM host.
#
# One run answers the whole question:
#
#   real repository
#   → ato build v1  (recipe producer → bootable rootfs → filesystem.view_digest
#                    → mint → capsule.lock atomic persist → trusted-load)
#   → KVM guest boot
#   → exact argv exec
#   → expected working directory
#   → readiness
#   → observed launch matches execution_id
#
# It builds TWICE and requires the identity to be one:
#
#   execution_id A          == execution_id B
#   filesystem.view_digest A == filesystem.view_digest B
#
# The packed ext4 bytes are NOT required to match, and a difference there is not
# a failure. `mke2fs` stamps every inode with the wall clock and ignores
# SOURCE_DATE_EPOCH (measured, e2fsprogs 1.47.0), which is exactly why the
# identity commits the guest's CONTENTS. Each artifact's digest is still
# recorded, and this checks that both builds reported one.
#
# Requires: Linux, root, /dev/kvm, docker, e2fsprogs, and a firecracker-capable
# host. Prints an evidence block to stdout for attaching to the PR or the ADR.
#
#   sudo ./scripts/e2e/adr015-v1-boot.sh /path/to/ato [/path/to/repo]
#
# Never prints secrets, host credentials or tokens: the evidence block is
# versions, digests, paths inside the fixture, and pass/fail.
set -euo pipefail

ATO_BIN="${1:-$(command -v ato || true)}"
REPO="${2:-}"

fail() { echo "FAIL: $*" >&2; exit 1; }
note() { echo "== $* =="; }

[[ -n "$ATO_BIN" ]] || fail "usage: $0 /path/to/ato [/path/to/repo]"
if [[ "$ATO_BIN" == */* ]]; then
  ATO_BIN="$(cd "$(dirname "$ATO_BIN")" && pwd)/$(basename "$ATO_BIN")"
else
  ATO_BIN="$(command -v "$ATO_BIN")"
fi
[[ -x "$ATO_BIN" ]] || fail "not executable: $ATO_BIN"

[[ "$(uname -s)" == "Linux" ]] || fail "step 6 needs Linux: mke2fs and KVM are under test"
[[ "$(id -u)" == "0" ]] || fail "run as root: 'docker export | tar -x' must restore ownership"
command -v docker >/dev/null || fail "docker not found"
command -v mke2fs >/dev/null || fail "e2fsprogs not found"

note "environment"
KERNEL="$(uname -srm)"
DOCKER_VERSION="$(docker --version)"
E2FS_VERSION="$(mke2fs -V 2>&1 | head -1)"
if [[ -e /dev/kvm ]]; then KVM="present"; else KVM="ABSENT"; fi
printf 'kernel       %s\n'  "$KERNEL"
printf 'docker       %s\n'  "$DOCKER_VERSION"
printf 'e2fsprogs    %s\n'  "$E2FS_VERSION"
printf '/dev/kvm     %s\n'  "$KVM"
printf 'ato          %s\n'  "$("$ATO_BIN" --version 2>&1 | head -1)"
[[ "$KVM" == "present" ]] || fail "/dev/kvm is absent; the boot half cannot run here"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# ── the repository under test ────────────────────────────────────────────────
#
# A v1 build refuses a Git working tree: a root-level `.git` is neither a
# control file the projection may withhold nor content whose bytes are
# reproducible. So an argument-supplied repo is EXPORTED rather than copied.
note "fixture"
FIXTURE="$WORK/fixture"
mkdir -p "$FIXTURE"
if [[ -n "$REPO" ]]; then
  REPO="$(cd "$REPO" && pwd)"
  SOURCE_COMMIT="$(git -C "$REPO" rev-parse HEAD)"
  git -C "$REPO" archive --format=tar HEAD | tar -x -C "$FIXTURE"
  printf 'repository   %s\n' "$REPO"
  printf 'commit       %s\n' "$SOURCE_COMMIT"
  [[ -f "$FIXTURE/capsule.toml" ]] || fail "$REPO has no capsule.toml at its root"
else
  SOURCE_COMMIT="(built-in fixture)"
  printf 'repository   %s\n' "$SOURCE_COMMIT"
  cat > "$FIXTURE/capsule.toml" <<'TOML'
schema_version = "1"
name = "adr015-step6"
version = "0.1.0"

[run]
command = ["python3", "server.py", "--label", "step 6"]

[web]
port = 8080
bind = "0.0.0.0"

[seal_at]
command = ["curl", "-fsS", "http://127.0.0.1:8080/"]
TOML
  # Echoes its own argv and cwd, so the boot check can compare what the guest
  # ACTUALLY ran against what the contract committed — rather than inferring it
  # from the fact that something answered.
  cat > "$FIXTURE/server.py" <<'PY'
import http.server, json, os, socketserver, sys

BODY = json.dumps({"argv": sys.argv, "cwd": os.getcwd()}).encode()

class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(BODY)))
        self.end_headers()
        self.wfile.write(BODY)
    def log_message(self, *_):
        pass

socketserver.TCPServer.allow_reuse_address = True
socketserver.TCPServer(("0.0.0.0", 8080), H).serve_forever()
PY
fi

# ── build twice ──────────────────────────────────────────────────────────────
run_build() {
  local label="$1" dir="$WORK/$label"
  note "build $label"
  cp -a "$FIXTURE" "$dir"
  # A fresh copy each time so neither build sees the other's capsule.lock as a
  # pre-existing control file — the withheld set is identity-bearing.
  ( cd "$dir" && "$ATO_BIN" --json build . ) >"$WORK/$label.out" 2>"$WORK/$label.err" || {
    echo "--- stdout ---"; cat "$WORK/$label.out"
    echo "--- stderr ---"; cat "$WORK/$label.err"
    fail "ato build ($label) failed"
  }
  python3 - "$WORK/$label.out" <<'PYEOF' >"$WORK/$label.facts"
import json, sys
text = open(sys.argv[1]).read()
decoder, found = json.JSONDecoder(), None
for start in range(len(text)):
    if text[start] != "{":
        continue
    try:
        value, _ = decoder.raw_decode(text[start:])
    except ValueError:
        continue
    if isinstance(value, dict) and value.get("v1"):
        found = value["v1"]
if not found:
    sys.exit("no v1 build result in --json output")
for key in ("execution_id", "filesystem_view_digest", "guest_image_digest",
            "source_digest", "guest_image", "lock", "target", "runtime"):
    print(found[key])
PYEOF
}

facts() { sed -n "${2}p" "$WORK/$1.facts"; }
run_build one
run_build two

read_facts() {
  local l="$1"
  ID="$(facts "$l" 1)"; VIEW="$(facts "$l" 2)"; ART="$(facts "$l" 3)"
  SRC="$(facts "$l" 4)"; IMG="$(facts "$l" 5)"; LOCK="$(facts "$l" 6)"
  TARGET="$(facts "$l" 7)"; RUNTIME="$(facts "$l" 8)"
  for v in "$ID" "$VIEW" "$ART" "$SRC" "$IMG" "$LOCK" "$TARGET" "$RUNTIME"; do
    [[ -n "$v" ]] || fail "build $l did not report one of its digests"
  done
}
read_facts one
ID_ONE="$ID"; VIEW_ONE="$VIEW"; ART_ONE="$ART"; SRC_ONE="$SRC"
IMG_ONE="$IMG"; LOCK_ONE="$LOCK"; TARGET_ONE="$TARGET"; RUNTIME_ONE="$RUNTIME"
read_facts two
ID_TWO="$ID"; VIEW_TWO="$VIEW"; ART_TWO="$ART"; SRC_TWO="$SRC"

note "identity across two builds"
printf 'source_digest          %s\n                       %s\n' "$SRC_ONE" "$SRC_TWO"
printf 'filesystem.view_digest %s\n                       %s\n' "$VIEW_ONE" "$VIEW_TWO"
printf 'execution_id           %s\n                       %s\n' "$ID_ONE" "$ID_TWO"
printf 'artifact digest        %s\n                       %s   (recorded, may differ)\n' \
  "$ART_ONE" "$ART_TWO"

[[ "$SRC_ONE"  == "$SRC_TWO"  ]] || fail "source.digest differs — the projection is not stable"
[[ "$VIEW_ONE" == "$VIEW_TWO" ]] || fail "filesystem.view_digest differs — the guest CONTENTS are not stable"
[[ "$ID_ONE"   == "$ID_TWO"   ]] || fail "the same program source minted two execution_ids"

# ── the lock the build published ─────────────────────────────────────────────
note "published lock"
[[ -f "$LOCK_ONE" ]] || fail "the reported lock does not exist: $LOCK_ONE"
LOCK_ID="$(python3 -c '
import json,sys
lock = json.load(open(sys.argv[1]))
env = lock.get("execution_contract") or {}
print(env.get("execution_id", ""))' "$LOCK_ONE")"
[[ "$LOCK_ID" == "$ID_ONE" ]] || fail "capsule.lock carries $LOCK_ID, the build reported $ID_ONE"
ARGV_COMMITTED="$(python3 -c '
import json,sys
lock = json.load(open(sys.argv[1]))
c = lock["execution_contract"]["execution_contract"]
print(json.dumps(c["launch"]["argv"]))' "$LOCK_ONE")"
CWD_COMMITTED="$(python3 -c '
import json,sys
lock = json.load(open(sys.argv[1]))
c = lock["execution_contract"]["execution_contract"]
print(c["launch"]["cwd"])' "$LOCK_ONE")"
printf 'lock                   %s\n' "$LOCK_ONE"
printf 'committed argv         %s\n' "$ARGV_COMMITTED"
printf 'committed cwd          %s\n' "$CWD_COMMITTED"

# ── boot it ──────────────────────────────────────────────────────────────────
#
# The guest is booted from the packed image and asked what it is running. The
# check is an EQUALITY against the contract, not "something answered": a guest
# that serves the right port while running a different argv is precisely the
# failure an Execution Identity exists to make impossible.
note "boot"
printf 'guest image            %s\n' "$IMG_ONE"
BOOT_RESULT="not-run"; READY_RESULT="not-run"
OBSERVED_ARGV="unknown"; OBSERVED_CWD="unknown"

if [[ -n "${ATO_STEP6_BOOT_CMD:-}" ]]; then
  # An explicit launcher wins: the harness on a given host knows how to boot a
  # rootfs there better than this script does. It receives the image path and
  # must expose the guest's port 8080 on the host as $ATO_STEP6_GUEST_URL.
  note "booting via ATO_STEP6_BOOT_CMD"
  ATO_STEP6_IMAGE="$IMG_ONE" bash -c "$ATO_STEP6_BOOT_CMD" &
  BOOT_PID=$!
  trap 'kill "$BOOT_PID" 2>/dev/null || true; rm -rf "$WORK"' EXIT
  GUEST_URL="${ATO_STEP6_GUEST_URL:-http://127.0.0.1:8080/}"
else
  fail "no launcher configured.

Set ATO_STEP6_BOOT_CMD to a command that boots \$ATO_STEP6_IMAGE under
firecracker on this host and exposes the guest's :8080, and optionally
ATO_STEP6_GUEST_URL (default http://127.0.0.1:8080/). Everything above this
point has already passed — build, identity across two builds, and the published
lock — so re-running with a launcher set resumes exactly here.

This script deliberately does not guess at a firecracker invocation: the kernel,
the network mode and the vsock layout differ per host, and a wrong guess would
report a boot failure that is the launcher's, not the capsule's."
fi

for _ in $(seq 1 60); do
  if BODY="$(curl -fsS --max-time 2 "$GUEST_URL" 2>/dev/null)"; then
    READY_RESULT="ready"
    break
  fi
  sleep 1
done
[[ "$READY_RESULT" == "ready" ]] || { BOOT_RESULT="no-readiness"; fail "the guest never answered $GUEST_URL"; }
BOOT_RESULT="booted"

OBSERVED_ARGV="$(printf '%s' "$BODY" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["argv"]))')"
OBSERVED_CWD="$(printf '%s' "$BODY" | python3 -c 'import json,sys; print(json.load(sys.stdin)["cwd"])')"

# The guest reports argv[0] as the script name python resolved, so compare the
# committed argv's TAIL — every word the author wrote after the interpreter.
python3 - "$ARGV_COMMITTED" "$OBSERVED_ARGV" <<'PYEOF' || fail "the guest ran a different argv than the contract committed"
import json, sys
committed = json.loads(sys.argv[1])
observed = json.loads(sys.argv[2])
# committed: ["python3", "server.py", "--label", "step 6"]
# observed : ["server.py", "--label", "step 6"]  (python drops the interpreter)
tail = committed[1:]
if observed != tail:
    print(f"committed tail {tail!r} != observed {observed!r}", file=sys.stderr)
    sys.exit(1)
PYEOF
[[ "$OBSERVED_CWD" == "$CWD_COMMITTED" ]] || \
  fail "the guest ran in $OBSERVED_CWD, the contract committed $CWD_COMMITTED"

# ── evidence ─────────────────────────────────────────────────────────────────
cat <<EVIDENCE

================ ADR-015 step 6 evidence ================
kernel                  $KERNEL
docker                  $DOCKER_VERSION
e2fsprogs               $E2FS_VERSION
/dev/kvm                $KVM
source commit           $SOURCE_COMMIT

execution ID            $ID_ONE
  second build          $ID_TWO            (equal)
filesystem view digest  $VIEW_ONE
  second build          $VIEW_TWO            (equal)
source digest           $SRC_ONE
rootfs artifact digest  $ART_ONE
  second build          $ART_TWO            (recorded; equality not required)
target                  $TARGET_ONE
runtime                 $RUNTIME_ONE

boot result             $BOOT_RESULT
readiness result        $READY_RESULT
exact argv (committed)  $ARGV_COMMITTED
exact argv (observed)   $OBSERVED_ARGV
working directory       $CWD_COMMITTED  (observed: $OBSERVED_CWD)
=========================================================
EVIDENCE
echo "PASS: one program source, one execution identity, and the guest ran it"
