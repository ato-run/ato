#!/usr/bin/env bash
# 6b-D2 offline source->OCI acceptance on a native Linux Docker host.
# Every build runs against a NEW private dockerd (empty classic store, own data
# root) inside a loopback-only network namespace. The host daemon is never
# given to the builder; it is read only once by the caller to acquire the
# frozen base archive. Runs reuse scripts/portable-offline-oci-airgap.sh.
#
# usage: sudo offline.sh ATO_BIN WORK_ROOT BASE_ARCHIVE RUN_USER CASE
#   CASE = build (fixture), or a negative case name.
set -euo pipefail
ATO="$1"; W="$2"; BASE="$3"; RUN_USER="$4"; CASE="$5"
REPO="$(cd "$(dirname "$0")/../../.." && pwd)"
if [[ "${INSIDE:-0}" != 1 ]]; then
  [[ -e "$W" ]] && { echo "refusing to reuse $W" >&2; exit 73; }
  install -d -o "$RUN_USER" "$W"
  exec unshare --net env INSIDE=1 "$0" "$@"
fi
ip link set lo up
[[ -z "$(ip route show default)" ]] || { echo "unexpected default route" >&2; exit 1; }
S="$W/docker.sock"
install -d -m 0710 "$W/data" "$W/exec"
dockerd --host "unix://$S" --data-root "$W/data" --exec-root "$W/exec" --pidfile "$W/dockerd.pid" \
  --feature containerd-snapshotter=false --group "$(id -gn "$RUN_USER")" --log-level warn >"$W/dockerd.log" 2>&1 &
P=$!
stop() { docker --host "unix://$S" ps -aq 2>/dev/null | xargs -r docker --host "unix://$S" rm -f >/dev/null 2>&1 || true; kill "$P" 2>/dev/null || true; wait "$P" 2>/dev/null || true; }
trap stop EXIT
for _ in $(seq 120); do docker --host "unix://$S" info >/dev/null 2>&1 && break; sleep .25; done
D=(docker --host "unix://$S")

CTX="$W/src/app"; install -d -o "$RUN_USER" "$W/src" "$CTX"
cp "$REPO/apps/portable-application/fixtures/source-oci/"* "$CTX/"
REF='python:3.12-alpine@sha256:c4634f578a412db396771b61b064c6e546c9d6414c7fb5b1b05d5871f1885f7b'
BASE_ARCHIVE="$BASE"; BASE_SHA="sha256:$(sha256sum "$BASE" | cut -d' ' -f1)"
TIMEOUT=600; MAXB=134217728; HOST="unix://$S"
case "$CASE" in
  build) ;;
  unauthorized_external_input) sed -i "s#^FROM .*#FROM alpine:3.20@sha256:$(printf '0%.0s' $(seq 64))#" "$CTX/Dockerfile" ;;
  context_escape) echo 'COPY ../outside.txt /srv/outside.txt' >> "$CTX/Dockerfile"; echo secret > "$W/src/outside.txt" ;;
  base_digest_mismatch) cp "$BASE" "$W/base-tampered.tar"; printf 'x' >> "$W/base-tampered.tar"; BASE_ARCHIVE="$W/base-tampered.tar" ;;
  build_timeout) sed -i 's#^COPY index.html#RUN sleep 120\nCOPY index.html#' "$CTX/Dockerfile"; TIMEOUT=5 ;;
  archive_bound) MAXB=1024 ;;
  build_network) sed -i 's#^COPY index.html#RUN wget -q -O /srv/remote.html http://example.com/\nCOPY index.html#' "$CTX/Dockerfile" ;;
  privileged_run) sed -i 's#^COPY index.html#RUN --security=insecure true\nCOPY index.html#' "$CTX/Dockerfile" ;;
  host_network_run) sed -i 's#^COPY index.html#RUN --network=host true\nCOPY index.html#' "$CTX/Dockerfile" ;;
  production_socket) HOST="unix:///var/run/docker.sock" ;;
  store_not_empty) "${D[@]}" load -i "$BASE" >/dev/null ;;
  *) echo "unknown case $CASE" >&2; exit 64 ;;
esac
( cd "$W/src" && tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -czf "$W/source.tar.gz" app )
SRC_SHA="sha256:$(sha256sum "$W/source.tar.gz" | cut -d' ' -f1)"
cat > "$W/request.json" <<JSON
{"schema":"ato.source-oci-request/1","title":"source-to-OCI fixture","source_archive":"$W/source.tar.gz",
 "source_archive_sha256":"$SRC_SHA","dockerfile":"Dockerfile","platform":"linux/amd64",
 "base_images":[{"reference":"$REF","archive":"$BASE_ARCHIVE","archive_sha256":"$BASE_SHA"}],
 "declared_transport_port":8080,
 "policy":{"network":"none","build_timeout_seconds":$TIMEOUT,"max_archive_bytes":$MAXB,
   "memory_bytes":268435456,"cpu_limit_millis":500,"pids_limit":128}}
JSON
chown -R "$RUN_USER" "$W/src" "$W/source.tar.gz" "$W/request.json"
set +e
runuser -u "$RUN_USER" -- env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy \
  "$ATO" __source-oci-build --request "$W/request.json" --docker-host "$HOST" --out "$W/out" >"$W/build.out" 2>&1
RC=$?
set -e
LEFT=$("${D[@]}" image ls -aq | wc -l); CONTAINERS=$("${D[@]}" ps -aq | wc -l)
if [[ "$CASE" == store_not_empty ]]; then LEFT=$((LEFT - 1)); fi
printf '{"case":"%s","exit":%d,"store_images_after":%d,"containers_after":%d,"route_default":%d}\n' \
  "$CASE" "$RC" "$LEFT" "$CONTAINERS" "$(ip route show default | wc -l)" > "$W/result.json"
tail -3 "$W/build.out" >> "$W/result.json"
cat "$W/result.json"
