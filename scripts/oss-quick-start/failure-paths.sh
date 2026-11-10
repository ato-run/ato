#!/usr/bin/env bash
# Negative paths for the local workspace: a checkpoint that cannot be written
# must keep the workspace running, and a busy local-data port must say what to
# do instead of failing anonymously.
#
# Usage: failure-paths.sh --ato <ato> --capsules <dir> --port <port>
set -euo pipefail

PYTHON=python
if python3 -c "import sys" >/dev/null 2>&1; then PYTHON=python3; fi

ato=ato
capsules=
port=18860
workdir=

while [ $# -gt 0 ]; do
  case "$1" in
    --ato) ato=$2; shift 2 ;;
    --capsules) capsules=$2; shift 2 ;;
    --port) port=$2; shift 2 ;;
    --workdir) workdir=$2; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$capsules" ] || { echo "--capsules is required" >&2; exit 2; }
workdir=${workdir:-$(mktemp -d "${TMPDIR:-/tmp}/ato-oss-failure.XXXXXX")}
for capsule in todo calendar excalidraw; do
  cp "$capsules/$capsule.capsule" "$workdir/$capsule.capsule"
done
digest() { shasum -a 256 "$1" | awk '{print "sha256:"$1}'; }
cat > "$workdir/workspace.toml" <<EOF
schema = "ato.workspace/1"

[layout]
direction = "horizontal"
split = [0.34, 0.33, 0.33]

[[pane]]
id = "todo"
app = "todo"
capsule = "todo.capsule"
digest = "$(digest "$workdir/todo.capsule")"

[[pane]]
id = "calendar"
app = "calendar"
capsule = "calendar.capsule"
digest = "$(digest "$workdir/calendar.capsule")"

[[pane]]
id = "excalidraw"
app = "excalidraw"
capsule = "excalidraw.capsule"
digest = "$(digest "$workdir/excalidraw.capsule")"
EOF

echo "== checkpoint write failure keeps the workspace up =="
# A regular file where the checkpoint directory would be makes the write fail
# on every platform (ENOTDIR), unlike a merely missing directory.
: > "$workdir/blocker"
"$ato" workspace open "$workdir/workspace.toml" --local-data --local-data-port "$port" \
  --no-open --checkpoint-out "$workdir/blocker/checkpoint.toml" >"$workdir/bad.log" 2>&1 &
pid=$!
for _ in $(seq 1 200); do
  url=$(grep -o 'http://127\.0\.0\.1:[0-9]*/' "$workdir/bad.log" | head -1 || true)
  [ -n "$url" ] && break
  kill -0 "$pid" 2>/dev/null || { echo "FAIL: workspace exited early"; exit 1; }
  sleep 0.05
done
[ -n "$url" ] || { echo "FAIL: no shell URL"; exit 1; }
html=$(curl -fsS "$url")
stop=$(grep -o 'http://127\.0\.0\.1:[0-9]*/stop?token=[a-f0-9]*' <<<"$html" | head -1)
status=$(grep -o 'http://127\.0\.0\.1:[0-9]*/status?token=[a-f0-9]*' <<<"$html" | head -1)
curl -fsS -X POST -o /dev/null "$stop"
sleep 1
kill -0 "$pid" 2>/dev/null || { echo "FAIL: workspace exited despite the failed checkpoint"; exit 1; }
body=$(curl -fsS "$status")
case "$body" in
  error:*) ;;
  *) echo "FAIL: expected an error status, got: $body"; kill -9 "$pid"; exit 1 ;;
esac
echo "ok: still running, status: $body"
kill -9 "$pid" 2>/dev/null || true

echo "== busy local-data port names the fix =="
"$PYTHON" - "$port" <<'PY' &
import socket, sys, time
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", int(sys.argv[1]))); s.listen(1); time.sleep(10)
PY
holder=$!
sleep 0.5
if "$ato" workspace open "$workdir/workspace.toml" --local-data --local-data-port "$port" --no-open 2>"$workdir/busy.log"; then
  echo "FAIL: the busy port was accepted"; kill -9 "$holder" 2>/dev/null; exit 1
fi
grep -q -- '--local-data-port' "$workdir/busy.log" || { echo "FAIL: no port hint: $(cat "$workdir/busy.log")"; exit 1; }
echo "ok: $(head -1 "$workdir/busy.log")"
kill "$holder" 2>/dev/null || true
echo
echo "PASS (failure paths)"
