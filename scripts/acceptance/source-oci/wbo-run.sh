#!/usr/bin/env bash
# Runs inside a new network namespace (see wbo.sh): a private Docker store,
# the saved WBO bundle imported as a durable local Instance, and the B-E
# checks fixed in docs/ops/formation-wbo-expectations.json.
#
# usage (from wbo.sh): unshare --net wbo-run.sh ATO_BIN OUT_ROOT RUN_USER DERIVATION
set -uo pipefail
ATO="$1"; OUT="$2"; RUN_USER="$3"; D="$4"
HERE="$(cd "$(dirname "$0")" && pwd)"
R="$OUT/run"; install -d -m 0750 -o "$RUN_USER" -g "$(id -gn "$RUN_USER")" "$R"
install -d -m 0710 "$R/docker-data" "$R/docker-exec"
ip link set lo up
[[ -z "$(ip route show default)" ]] || { echo "unexpected default route"; exit 1; }
S="$R/docker.sock"
dockerd --host "unix://$S" --data-root "$R/docker-data" --exec-root "$R/docker-exec" \
  --pidfile "$R/dockerd.pid" --feature containerd-snapshotter=false \
  --group "$(id -gn "$RUN_USER")" --log-level warn > "$R/dockerd.log" 2>&1 &
DP=$!
stop_all() {
  docker --host "unix://$S" ps -aq 2>/dev/null | xargs -r docker --host "unix://$S" rm -f >/dev/null 2>&1
  kill "$DP" 2>/dev/null; wait "$DP" 2>/dev/null
}
trap stop_all EXIT
for _ in $(seq 120); do docker --host "unix://$S" info >/dev/null 2>&1 && break; sleep 0.25; done
DK=(docker --host "unix://$S")
[[ -z "$("${DK[@]}" image ls -q)" ]] || { echo "store not empty"; exit 1; }
U=(runuser -u "$RUN_USER" -- env -i PATH=/usr/bin:/bin HOME="$OUT/home"
   ATO_HOME="$OUT/ato-home" DOCKER_HOST="unix://$S")
CHECK=(runuser -u "$RUN_USER" -- env -i PATH=/usr/bin:/bin HOME="$OUT/home"
   node "$HERE/wbo-check.mjs")
CHROME=/usr/bin/google-chrome

"${U[@]}" "$ATO" app import "$OUT/offline.capsule" --derivation "$D" > "$R/import.json" 2> "$R/import.err"
INSTANCE=$(python3 -c 'import json,sys;d=json.load(open(sys.argv[1]));print(d.get("instance_id") or d["id"])' "$R/import.json" 2>/dev/null)
[[ -n "$INSTANCE" ]] || { echo "B import failed: $(tail -3 "$R/import.err")"; exit 1; }

start_run() { # $1 = n
  "${U[@]}" "$ATO" app start "$INSTANCE" --no-open --verification-receipt "$R/receipt-$1.json" \
    > "$R/start-$1.txt" 2>&1
  local rc=$?
  URL=$(sed -n 's/^URL: //p' "$R/start-$1.txt")
  echo "B start-$1 exit=$rc url=$URL $( [[ $rc -ne 0 ]] && tail -3 "$R/start-$1.txt" | tr '\n' ' ')"
  [[ $rc -eq 0 ]] || return 1
  echo "C receipt-$1 $(python3 -c 'import json,sys;r=json.load(open(sys.argv[1]));print(json.dumps({"fully_satisfied":r["fully_satisfied"],"outcomes":[o["outcome"] for o in r["observations"]],"contract_ref":r["contract_ref"],"derivation_ref":r["derivation_ref"]}))' "$R/receipt-$1.json")"
}
identity() {
  local c; c=$("${DK[@]}" ps -q | head -1)
  echo "identity container=$c user=$("${DK[@]}" inspect --format '{{.Config.User}}' "$c") procs=$("${DK[@]}" top "$c" -eo uid,gid,comm | tail -n +2 | tr -s ' ' | tr '\n' ';') mounts=$("${DK[@]}" inspect --format '{{json .Mounts}}' "$c")"
}

if start_run 1; then
  identity
  echo "D $("${CHECK[@]}" draw --chrome "$CHROME" --url "$URL" --board ato-wbo-check --id ato-wbo-probe-1 --scratch "$OUT/home" 2>&1 | tail -1)"
  "${U[@]}" "$ATO" app stop "$INSTANCE" > "$R/stop-1.txt" 2>&1; echo "B stop-1 exit=$?"
  if start_run 2; then
    echo "E $("${CHECK[@]}" verify --chrome "$CHROME" --url "$URL" --board ato-wbo-check --id ato-wbo-probe-1 --scratch "$OUT/home" 2>&1 | tail -1)"
    "${U[@]}" "$ATO" app stop "$INSTANCE" > "$R/stop-2.txt" 2>&1; echo "B stop-2 exit=$?"
  fi
fi
FILES=$(grep -rl "ato-wbo-probe-1" "$OUT/ato-home" 2>/dev/null | grep -v -e receipt -e '\.log$' | head -5)
echo "state files_with_probe=[$(echo "$FILES" | tr '\n' ' ')] owners=[$(for f in $FILES; do stat -c '%u:%g' "$f"; done | sort -u | tr '\n' ' ')]"
echo "store containers=$("${DK[@]}" ps -aq | wc -l) volumes=$("${DK[@]}" volume ls -q | wc -l)"
