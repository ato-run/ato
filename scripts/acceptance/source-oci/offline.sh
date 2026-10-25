#!/usr/bin/env bash
# 6b-D2 offline source->OCI acceptance on a native Linux Docker host.
# `ato __source-oci-build` itself starts the private builder session (own
# work root, loop filesystem, cgroup with limits, network namespace with only
# lo, socket); this script only prepares inputs, runs one case and records
# what remains afterwards. The host daemon is never given to the builder.
#
# usage: sudo offline.sh ATO_BIN WORK_ROOT BASE_ARCHIVE RUN_USER CASE
#   CASE = build (fixture), or a negative case name.
set -euo pipefail
ATO="$1"; W="$2"; BASE="$3"; RUN_USER="$4"; CASE="$5"
REPO="$(cd "$(dirname "$0")/../../.." && pwd)"
[[ $EUID -eq 0 ]] || { echo "run as root" >&2; exit 77; }
[[ -e "$W" ]] && { echo "refusing to reuse $W" >&2; exit 73; }
install -d -o "$RUN_USER" "$W"
SESSION_PARENT=/srv/ato-src; install -d -m 0700 "$SESSION_PARENT"
SESSION="$SESSION_PARENT/$CASE-$$"

CTX="$W/src/app"; install -d -o "$RUN_USER" "$W/src" "$CTX"
cp "$REPO/apps/portable-application/fixtures/source-oci/"* "$CTX/"
ROOT='sha256:c4634f578a412db396771b61b064c6e546c9d6414c7fb5b1b05d5871f1885f7b'
REF="python:3.12-alpine@$ROOT"
BASE_ARCHIVE="$BASE"
TIMEOUT=600; MAXB=134217728; DISK=4294967296; MEM=2147483648; PIDS=1024
HOST_IP=$(ip -4 route get 1.1.1.1 | sed -n 's/.* src \([0-9.]*\).*/\1/p')
SERVER_PID=""; WATCH_PID=""; FROZEN_SHA=""
insert_before_copy() { # $1 = Dockerfile lines inserted before COPY index.html
  python3 - "$CTX/Dockerfile" "$1" <<'PY'
import sys
p, extra = sys.argv[1], sys.argv[2]
s = open(p).read()
assert "COPY index.html" in s
open(p, "w").write(s.replace("COPY index.html", extra + "\nCOPY index.html", 1))
PY
}
case "$CASE" in
  build) ;;
  unauthorized_external_input) sed -i "s#^FROM .*#FROM alpine:3.20@sha256:$(printf '0%.0s' $(seq 64))#" "$CTX/Dockerfile" ;;
  context_escape) insert_before_copy 'COPY ../outside.txt /srv/outside.txt'; echo secret > "$W/src/outside.txt" ;;
  base_digest_mismatch)
    # The request keeps the frozen digest of the original bytes; the archive
    # it names has one byte appended (still a readable tar).
    FROZEN_SHA="sha256:$(sha256sum "$BASE" | cut -d' ' -f1)"
    cp "$BASE" "$W/base-tampered.tar"; printf 'x' >> "$W/base-tampered.tar"; BASE_ARCHIVE="$W/base-tampered.tar" ;;
  base_graph_mismatch)
    # Same member set, but Docker's load manifest names a config the pinned
    # root never reaches (the attestation manifest blob).
    python3 - "$BASE" "$W/base-graph.tar" <<'PY'
import io, json, sys, tarfile
src, dst = sys.argv[1], sys.argv[2]
with tarfile.open(src) as t, tarfile.open(dst, "w") as o:
    for m in t.getmembers():
        data = t.extractfile(m).read() if m.isfile() else None
        if m.name == "manifest.json":
            legacy = json.loads(data)
            legacy[0]["Config"] = "blobs/sha256/71474a52ec5920a96fad77e8ba4618a3a4bee17396eec4c73b3ed9bd46dec595"
            data = json.dumps(legacy).encode()
            m.size = len(data)
        o.addfile(m, io.BytesIO(data) if data is not None else None)
PY
    BASE_ARCHIVE="$W/base-graph.tar" ;;
  build_timeout) insert_before_copy 'RUN sleep 120'; TIMEOUT=5 ;;
  client_disconnect)
    # The client is killed while a RUN step is running in the daemon; the
    # session must still stop that step (no process left in its cgroup).
    insert_before_copy 'RUN sleep 120'
    ( for _ in $(seq 600); do
        if grep -ls "ato-source-oci-.*/builds/" /proc/[0-9]*/cgroup 2>/dev/null | head -1 | grep -q .; then
          pkill -KILL -f -- "buildx build --builder default" && echo killed > "$W/client-killed"; break
        fi; sleep 0.2; done ) & WATCH_PID=$! ;;
  archive_bound) MAXB=1024 ;;
  disk_bound) DISK=536870912; insert_before_copy 'RUN dd if=/dev/zero of=/fill bs=1M count=1024' ;;
  log_bound)
    # BuildKit clips each step's output (size and rate), so the bound is
    # reached through many steps, each printing ~100 KB.
    L='yes aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa | head -c 100000'
    STEPS=""; for i in $(seq 60); do STEPS+="RUN $L && echo step-$i"$'\n'; done
    insert_before_copy "${STEPS%$'\n'}" ;;
  build_network) insert_before_copy 'RUN wget -q -O /srv/remote.html http://example.com/' ;;
  privileged_run) insert_before_copy 'RUN --security=insecure true' ;;
  host_network_run) insert_before_copy 'RUN --network=host true' ;;
  remote_input_reachable)
    # A server the HOST can reach: a remote build input and an unfrozen
    # registry image on it must not be fetched by the session.
    install -d "$W/www"; echo hello > "$W/www/x"
    ( cd "$W/www" && exec python3 -m http.server 18099 --bind "$HOST_IP" ) > "$W/server.log" 2>&1 & SERVER_PID=$!
    for _ in $(seq 50); do curl -fs "http://$HOST_IP:18099/x" >/dev/null && break; sleep 0.2; done
    : > "$W/server.log"   # the reachability probe above is not counted
    insert_before_copy "ADD http://$HOST_IP:18099/x /srv/remote.txt" ;;
  remote_registry_reachable)
    install -d "$W/www"
    ( cd "$W/www" && exec python3 -m http.server 18099 --bind "$HOST_IP" ) > "$W/server.log" 2>&1 & SERVER_PID=$!
    for _ in $(seq 50); do curl -s -o /dev/null "http://$HOST_IP:18099/" && break; sleep 0.2; done
    : > "$W/server.log"
    sed -i "s#^FROM .*#FROM $HOST_IP:18099/library/python:3.12-alpine#" "$CTX/Dockerfile" ;;
  socket_alias|non_root) ;;
  *) echo "unknown case $CASE" >&2; exit 64 ;;
esac
BASE_SHA="${FROZEN_SHA:-sha256:$(sha256sum "$BASE_ARCHIVE" | cut -d' ' -f1)}"
( cd "$W/src" && tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -czf "$W/source.tar.gz" app )
SRC_SHA="sha256:$(sha256sum "$W/source.tar.gz" | cut -d' ' -f1)"
cat > "$W/request.json" <<JSON
{"schema":"ato.source-oci-request/1","title":"source-to-OCI fixture","source_archive":"$W/source.tar.gz",
 "source_archive_sha256":"$SRC_SHA","dockerfile":"Dockerfile","platform":"linux/amd64",
 "base_images":[{"reference":"$REF","pinned_digest":"$ROOT","archive":"$BASE_ARCHIVE","archive_sha256":"$BASE_SHA"}],
 "declared_transport_port":8080,
 "policy":{"network":"none","build_timeout_seconds":$TIMEOUT,"max_archive_bytes":$MAXB,
   "build":{"memory_bytes":$MEM,"cpu_limit_millis":2000,"pids_limit":$PIDS,"disk_bytes":$DISK},
   "runtime":{"memory_bytes":268435456,"cpu_limit_millis":500,"pids_limit":128}}}
JSON
chmod -R a+rX "$W/src" "$W/source.tar.gz" "$W/request.json" "$BASE_ARCHIVE"
ARGS=(__source-oci-build --request "$W/request.json" --work-root "$SESSION" --out "$W/out")
set +e
case "$CASE" in
  non_root) runuser -u "$RUN_USER" -- env -i PATH=/usr/bin:/bin "$ATO" "${ARGS[@]}" >"$W/build.out" 2>&1 ;;
  socket_alias)
    ln -s /var/run/docker.sock "$W/alias.sock"
    env -i PATH=/usr/bin:/bin "$ATO" "${ARGS[@]}" --docker-host "unix://$W/alias.sock" >"$W/build.out" 2>&1 ;;
  *) env -i PATH=/usr/bin:/bin "$ATO" "${ARGS[@]}" >"$W/build.out" 2>&1 ;;
esac
RC=$?
set -e
if [[ -n "$WATCH_PID" ]]; then wait "$WATCH_PID" 2>/dev/null || true; fi
if [[ -n "$SERVER_PID" ]]; then kill "$SERVER_PID" 2>/dev/null || true; wait "$SERVER_PID" 2>/dev/null || true; fi
# What remains of the session after the command returned.
set +o pipefail
CGROUPS=$(find /sys/fs/cgroup -maxdepth 1 -name 'ato-source-oci-*' | wc -l)
PROCS=$(grep -ls "ato-source-oci-" /proc/[0-9]*/cgroup 2>/dev/null | wc -l)
MOUNTS=$(awk -v p="$SESSION" 'index($5, p) == 1' /proc/self/mountinfo | wc -l)
LOOPS=$(grep -l "$SESSION" /sys/block/loop*/loop/backing_file 2>/dev/null | wc -l)
set -o pipefail
REQS=0; [[ -f "$W/server.log" ]] && REQS=$(grep -c '"GET\|"HEAD' "$W/server.log" || true)
b() { if [[ -e "$1" ]]; then echo true; else echo false; fi; }
printf '{"case":"%s","exit":%d,"session_root_left":%s,"session_cgroups_left":%d,"session_processes_left":%d,"session_mounts_left":%d,"session_loop_devices_left":%d,"artifact_published":%s,"client_killed":%s,"remote_requests_during_build":%d}\n' \
  "$CASE" "$RC" "$(b "$SESSION")" "$CGROUPS" "$PROCS" "$MOUNTS" "$LOOPS" "$(b "$W/out")" "$(b "$W/client-killed")" "$REQS" > "$W/result.json"
{ cat "$W/result.json"; grep -m1 "^error" "$W/build.out" || tail -3 "$W/build.out"; } | tee "$W/result.txt"
chown -R "$RUN_USER" "$W"
