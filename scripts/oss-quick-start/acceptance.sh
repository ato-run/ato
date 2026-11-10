#!/usr/bin/env bash
# Local acceptance for the OSS quick start: open a 3-pane Workspace through the
# in-process Coordinator, drive the sample operations, checkpoint, resume, and
# report the measured phase times.
#
# It does NOT install the CLI. Installation is a per-host step (Homebrew, MSI,
# shell installer) and is recorded separately; pass --install-cmd to include it
# in the measurement.
#
# Usage:
#   acceptance.sh --ato <ato> --capsules <dir> [--install-cmd <cmd>]
#
# <dir> must contain todo.capsule, calendar.capsule and excalidraw.capsule.
set -euo pipefail

ato=ato
capsules=
install_cmd=
port=18840
workdir=

while [ $# -gt 0 ]; do
  case "$1" in
    --ato) ato=$2; shift 2 ;;
    --capsules) capsules=$2; shift 2 ;;
    --install-cmd) install_cmd=$2; shift 2 ;;
    --port) port=$2; shift 2 ;;
    --workdir) workdir=$2; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

[ -n "$capsules" ] || { echo "--capsules is required" >&2; exit 2; }
for capsule in todo calendar excalidraw; do
  [ -f "$capsules/$capsule.capsule" ] || { echo "missing $capsules/$capsule.capsule" >&2; exit 2; }
done

now_ms() { python3 -c 'import time; print(int(time.time()*1000))'; }
fail() { echo "FAIL: $*" >&2; exit 1; }
digest() { shasum -a 256 "$1" | awk '{print "sha256:"$1}'; }

workdir=${workdir:-$(mktemp -d "${TMPDIR:-/tmp}/ato-oss-acceptance.XXXXXX")}
mkdir -p "$workdir"
echo "workdir: $workdir"
for capsule in todo calendar excalidraw; do
  cp "$capsules/$capsule.capsule" "$workdir/$capsule.capsule"
done

install_ms=0
if [ -n "$install_cmd" ]; then
  echo "install: $install_cmd"
  started=$(now_ms)
  bash -c "$install_cmd" || fail "installation failed"
  install_ms=$(( $(now_ms) - started ))
  echo "install: ${install_ms}ms"
fi

cat > "$workdir/workspace.toml" <<EOF
schema = "ato.workspace/1"

[layout]
direction = "horizontal"
split = [0.4, 0.3, 0.3]

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

start_workspace() {
  local log=$1
  shift
  : > "$log"
  "$ato" workspace open "$workdir/workspace.toml" --local-data --local-data-port "$port" --no-open "$@" >"$log" 2>&1 &
  started_pid=$!
}

wait_shell() {
  local pid=$1 log=$2 url
  url=
  while [ -z "$url" ]; do
    url=$(grep -o 'http://127\.0\.0\.1:[0-9]*/' "$log" | head -1 || true)
    [ -n "$url" ] || { kill -0 "$pid" 2>/dev/null || fail "workspace exited before serving"; sleep 0.05; }
  done
  while :; do
    if curl -fsS "$url" >/dev/null 2>&1; then break; fi
    kill -0 "$pid" 2>/dev/null || fail "workspace exited before the shell answered"
    sleep 0.05
  done
  echo "$url"
}

stop_workspace() { # pid html
  local pid=$1 html=$2 stop
  stop=$(grep -o 'http://127\.0\.0\.1:[0-9]*/stop?token=[a-f0-9]*' <<<"$html" | head -1)
  if [ -n "$stop" ]; then
    curl -fsS -X POST -o /dev/null "$stop" || true
    for _ in $(seq 1 100); do
      kill -0 "$pid" 2>/dev/null || return 0
      sleep 0.1
    done
  fi
  kill -INT "$pid" 2>/dev/null || true
}

invoke() { # pane port operation arguments
  curl -fsS -X POST "http://127.0.0.1:$port/apps/$1/__ato/data-ports/$2/invoke" \
    -H 'content-type: application/json' \
    -d "{\"operation\":\"$3\",\"arguments\":$4,\"idempotency_key\":\"$RANDOM-$RANDOM\"}"
}

bridge() { curl -fsS "http://127.0.0.1:$port/apps/$1/__ato/data-ports"; }

echo "== fresh open =="
started=$(now_ms)
start_workspace "$workdir/fresh.log" --checkpoint-out "$workdir/checkpoint.toml"
pid=$started_pid
url=$(wait_shell "$pid" "$workdir/fresh.log")
open_ms=$(( $(now_ms) - started ))
echo "shell ready: ${open_ms}ms $url"

html=$(curl -fsS "$url")
iframes=$(grep -o '<iframe' <<<"$html" | wc -l | tr -d ' ')
[ "$iframes" -ge 3 ] || fail "expected at least 3 pane iframes, saw $iframes"

todo=$(bridge todo)
grep -q '"read_write"' <<<"$todo" || fail "TODO bridge is not read_write: $todo"
calendar=$(bridge calendar)
grep -q '"operations"' <<<"$calendar" || fail "Calendar bridge is not operation-only: $calendar"
grep -q 'todo_list_due_dates' <<<"$calendar" || fail "Calendar bridge does not list todo_list_due_dates"
bridge excalidraw | grep -q '"scene"' || fail "Excalidraw bridge has no scene binding"

invoke todo tasks todo_create '{"at":"2026-11-10T09:00:00.000Z","title":"oss quick start","due_at":"2026-12-24"}' >/dev/null
projection=$(invoke calendar tasks todo_list_due_dates '{}')
grep -q '"title": "oss quick start"' <<<"$projection" || grep -q '"title":"oss quick start"' <<<"$projection" || fail "Calendar projection is missing the created task: $projection"
invoke excalidraw scene scene_load '{}' >/dev/null

stop_workspace "$pid" "$html"
wait "$pid" || true
[ -f "$workdir/checkpoint.toml" ] || fail "no checkpoint was written"

echo "== resume =="
started=$(now_ms)
start_workspace "$workdir/resume.log" --checkpoint "$workdir/checkpoint.toml"
pid=$started_pid
url=$(wait_shell "$pid" "$workdir/resume.log")
resume_ms=$(( $(now_ms) - started ))
echo "shell ready: ${resume_ms}ms $url"

projection=$(invoke calendar tasks todo_list_due_dates '{}')
grep -q '"title"' <<<"$projection" || fail "Calendar projection is empty after resume: $projection"
grep -q 'oss quick start' <<<"$projection" || fail "Calendar projection lost the task after resume: $projection"
invoke excalidraw scene scene_load '{}' >/dev/null

html=$(curl -fsS "$url")
iframes=$(grep -o '<iframe' <<<"$html" | wc -l | tr -d ' ')
[ "$iframes" -ge 3 ] || fail "resume shell lost panes (iframes=$iframes)"
grep -q '0.4' <<<"$html" || fail "resume shell lost the saved split"

stop_workspace "$pid" "$html"
wait "$pid" || true

total_ms=$(( install_ms + open_ms + resume_ms ))
echo
echo "PASS"
echo "  install: ${install_ms}ms"
echo "  open:    ${open_ms}ms"
echo "  resume:  ${resume_ms}ms"
echo "  total:   ${total_ms}ms (excluding sample acquisition)"
