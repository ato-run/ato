#!/usr/bin/env bash
# OCI profile acceptance: optional `oci.working_dir`, a `[[state]]` slot over
# an image VOLUME, and no anonymous volumes. Runs the existing pack -> offline
# export -> air-gapped run path (scripts/portable-offline-oci-airgap.sh).
#
# usage: sudo offline.sh ATO_BIN OUT_ROOT IMAGE_ARCHIVE IMAGE_REF RUN_USER
#   IMAGE_ARCHIVE/IMAGE_REF: the fixture/ image, built by the source->OCI
#   materializer (portable transport form, digest-pinned reference).
#
# Cases (each a new bundle, run from an empty private store):
#   declared  working_dir /opt/app + state at the VOLUME; K also pins the
#             body of /whoami (uid, gid, cwd, state writable)  -> PASS
#   no_state  working_dir only; the image VOLUME is uncovered -> refused
#   legacy    state only; no working_dir, so /app              -> not satisfied
set -euo pipefail
ATO="$1"; OUT="$2"; ARCHIVE="$3"; IMAGE="$4"; RUN_USER="$5"
REPO="$(cd "$(dirname "$0")/../../.." && pwd)"
[[ $EUID -eq 0 ]] || { echo "run as root" >&2; exit 77; }
[[ -e "$OUT" ]] && { echo "refusing to reuse $OUT" >&2; exit 73; }
install -d -o "$RUN_USER" "$OUT"
WHOAMI_DIGEST="sha256:256950ca71b4af3b2e315664a11fbcc3cad2bf62dfdc561c9cc247b232bc2819"

author() { # $1 case dir, $2 working_dir line, $3 state block, $4 extra observation
  install -d -o "$RUN_USER" "$1/src"
  echo "OCI profile acceptance workspace" > "$1/src/README.txt"
  cat > "$1/src/capsule.toml" <<TOML
schema = "ato.capsule/2"

[application]
title = "OCI profile acceptance"
surface_path = "/"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."
$3
[contract]
mode = "all"

[[contract.observation]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
path = "/"
status = 200
$4
[[derivation]]
id = "oci"
use = "ato.oci@1"
argv = ["python3", "server.py"]
cwd = "."
guest_port = 8080
runtimes = { "oci.image" = "$IMAGE", "oci.platform" = "linux/amd64", "oci.memory_bytes" = "268435456", "oci.cpu_limit_millis" = "500", "oci.pids_limit" = "128", "oci.workspace_mount" = "/.ato-workspace"$2 }

[effects]
default = "pure"
TOML
  chown -R "$RUN_USER" "$1"
}
STATE='
[[state]]
id = "server-data"
use = "ato.state.filesystem@1"
mount = "/opt/app/server-data"
access = "read-write"
'
WHOAMI="
[[contract.observation]]
id = \"whoami\"
use = \"ato.contract.http@1\"
port = \"app.http\"
path = \"/whoami\"
status = 200
body_digest = \"$WHOAMI_DIGEST\"
"
WD=', "oci.working_dir" = "/opt/app"'

run_case() { # $1 name
  local C="$OUT/$1"
  runuser -u "$RUN_USER" -- "$ATO" pack "$C/src" -o "$C/app.capsule" > "$C/pack.txt"
  local D; D=$(sed -n 's/^derivation_ref=//p' "$C/pack.txt")
  runuser -u "$RUN_USER" -- "$ATO" export "$C/app.capsule" --portability offline \
    --oci-archive "$ARCHIVE" -o "$C/offline.capsule" > "$C/export.txt"
  set +e
  "$REPO/scripts/portable-offline-oci-airgap.sh" --ato "$ATO" --bundle "$C/offline.capsule" \
    --derivation "$D" --work-root "$C/run" --run-user "$RUN_USER" > "$C/run.log" 2>&1
  local RC=$?
  set -e
  local VOLUMES
  if [[ -f "$C/run/result.txt" ]]; then VOLUMES=$(sed -n 's/^volume_count=//p' "$C/run/result.txt")
  else VOLUMES=$(grep -c . "$C/run/failure-volumes.txt" 2>/dev/null || true); fi
  printf '{"case":"%s","exit":%d,"derivation_ref":"%s","private_store_volumes":%s}\n' \
    "$1" "$RC" "$D" "${VOLUMES:-null}" | tee "$C/result.json"
  grep -m2 -iE "anonymous volumes|not satisfied|failed|error" "$C/run.log" | cut -c1-300 || true
}

author "$OUT/declared" "$WD" "$STATE" "$WHOAMI"; run_case declared
author "$OUT/no_state" "$WD" "" ""; run_case no_state
author "$OUT/legacy" "" "$STATE" ""; run_case legacy
chown -R "$RUN_USER" "$OUT"
