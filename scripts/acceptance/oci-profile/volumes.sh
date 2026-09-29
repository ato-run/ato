#!/usr/bin/env bash
# Image VOLUME coverage on a real Docker daemon: a VOLUME is allowed only when
# a declared mount has exactly its path. Each case packs a route over a
# prebuilt image, exports it offline and runs it air-gapped from an empty
# private store (scripts/portable-offline-oci-airgap.sh). While the run is live
# the private daemon's containers are inspected; afterwards its volumes are
# listed. Refused cases must stop before a container is created.
#
# usage: sudo volumes.sh ATO_BIN OUT_ROOT CASES_FILE RUN_USER
#   CASES_FILE lines: NAME IMAGE_ARCHIVE IMAGE_REF STATE_MOUNT|- EXPECT(pass|refuse)
set -euo pipefail
ATO="$1"; OUT="$2"; CASES="$3"; RUN_USER="$4"
REPO="$(cd "$(dirname "$0")/../../.." && pwd)"
[[ $EUID -eq 0 ]] || { echo "run as root" >&2; exit 77; }
[[ -e "$OUT" ]] && { echo "refusing to reuse $OUT" >&2; exit 73; }
install -d -o "$RUN_USER" "$OUT"

while read -r NAME ARCHIVE IMAGE STATE EXPECT; do
  [[ -z "$NAME" || "$NAME" == \#* ]] && continue
  C="$OUT/$NAME"; install -d -o "$RUN_USER" "$C/src"
  echo "VOLUME coverage acceptance workspace" > "$C/src/README.txt"
  STATE_BLOCK=""
  if [[ "$STATE" != "-" ]]; then
    STATE_BLOCK="
[[state]]
id = \"data\"
use = \"ato.state.filesystem@1\"
mount = \"$STATE\"
access = \"read-write\"
"
  fi
  cat > "$C/src/capsule.toml" <<TOML
schema = "ato.capsule/2"

[application]
title = "VOLUME coverage $NAME"
surface_path = "/"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."
$STATE_BLOCK
[contract]
mode = "all"

[[contract.observation]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
path = "/"
status = 200

[[derivation]]
id = "oci"
use = "ato.oci@1"
argv = ["python3", "-m", "http.server", "8080", "--directory", "/usr"]
cwd = "."
guest_port = 8080
runtimes = { "oci.image" = "$IMAGE", "oci.platform" = "linux/amd64", "oci.memory_bytes" = "268435456", "oci.cpu_limit_millis" = "500", "oci.pids_limit" = "128", "oci.workspace_mount" = "/.ato-workspace" }

[effects]
default = "pure"
TOML
  chown -R "$RUN_USER" "$C"
  runuser -u "$RUN_USER" -- "$ATO" pack "$C/src" -o "$C/app.capsule" > "$C/pack.txt"
  D=$(sed -n 's/^derivation_ref=//p' "$C/pack.txt")
  runuser -u "$RUN_USER" -- "$ATO" export "$C/app.capsule" --portability offline \
    --oci-archive "$ARCHIVE" -o "$C/offline.capsule" > "$C/export.txt"
  # Record every container the private daemon ever shows, with its mounts.
  ( S="$C/run/docker.sock"; : > "$C/containers-seen.jsonl"
    for _ in $(seq 900); do
      if [[ -S "$S" ]]; then
        for id in $(docker --host "unix://$S" ps -aq 2>/dev/null); do
          docker --host "unix://$S" inspect --format '{"id":"{{.Id}}","mounts":{{json .Mounts}}}' "$id" \
            >> "$C/containers-seen.jsonl" 2>/dev/null || true
        done
      fi
      [[ -f "$C/run.done" ]] && break; sleep 0.2
    done ) & WATCH=$!
  set +e
  "$REPO/scripts/portable-offline-oci-airgap.sh" --ato "$ATO" --bundle "$C/offline.capsule" \
    --derivation "$D" --work-root "$C/run" --run-user "$RUN_USER" > "$C/run.log" 2>&1
  RC=$?
  set -e
  touch "$C/run.done"; wait "$WATCH" || true
  if [[ -f "$C/run/result.txt" ]]; then VOLUMES=$(sed -n 's/^volume_count=//p' "$C/run/result.txt")
  else VOLUMES=$(grep -c . "$C/run/failure-volumes.txt" 2>/dev/null || true); fi
  CONTAINERS=$(cut -d'"' -f4 "$C/containers-seen.jsonl" | sort -u | grep -c . || true)
  ANON=$(grep -o '"Type":"volume"' "$C/containers-seen.jsonl" | wc -l)
  CODE=$(grep -o 'oci_volume_unauthorized' "$C/run.log" | head -1 || true)
  printf '{"case":"%s","expect":"%s","exit":%d,"code":"%s","containers_created":%d,"volume_mounts_seen":%d,"private_store_volumes":%s,"derivation_ref":"%s"}\n' \
    "$NAME" "$EXPECT" "$RC" "$CODE" "$CONTAINERS" "$ANON" "${VOLUMES:-null}" "$D" | tee "$C/result.json"
done < "$CASES"
chown -R "$RUN_USER" "$OUT"
