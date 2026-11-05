#!/bin/sh
# Isolated synthetic integration proof; never use real user data or credentials.
set -eu

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
tag=$(python3 -c 'import secrets; print(secrets.token_hex(6))')
network="fleetbase-socket-proof-$tag"
volume="fleetbase-socket-proof-data-$tag"
app="fleetbase-socket-proof-app-$tag"
console="fleetbase-socket-proof-console-$tag"
mock="fleetbase-socket-proof-mock-$tag"
app_image=${FLEETBASE_SOCKET_PROOF_APP_IMAGE:-ato-local-proof/fleetbase-combined:socket-roundtrip}
console_image=${FLEETBASE_SOCKET_PROOF_CONSOLE_IMAGE:-ato-local-proof/fleetbase-console:origin-bound-arm64}
mock_image=${FLEETBASE_SOCKET_PROOF_MOCK_IMAGE:-ato-local-proof/fleetbase-breach-proxy:arm64-v2}

cleanup() {
  docker stop --timeout 2 "$console" "$mock" >/dev/null 2>&1 || true
  docker stop --timeout 45 "$app" >/dev/null 2>&1 || true
  docker rm -v "$console" "$mock" "$app" >/dev/null 2>&1 || true
  docker volume rm "$volume" >/dev/null 2>&1 || true
  docker network rm "$network" >/dev/null 2>&1 || true
}
trap cleanup EXIT HUP INT TERM

docker network create --internal "$network" >/dev/null
docker volume create "$volume" >/dev/null
docker run --rm --network none --user 0:0 --read-only \
  --mount "type=volume,src=$volume,dst=/data,volume-nocopy" --entrypoint sh "$app_image" \
  -c 'chown "$1:$2" /data && chmod 700 /data' sh "$(id -u)" "$(id -g)"

# The breach service is a deliberate fake used exclusively with generated
# synthetic credentials, on an internal Docker network with no external egress.
docker run -d --name "$mock" --network "$network" --network-alias breachcheck \
  --read-only --cap-drop ALL --security-opt no-new-privileges --user 65532:65532 \
  --entrypoint python3 "$mock_image" -c \
  'import sys;sys.path.insert(0,"/opt");import breach_proxy as p;p.fetch_range=lambda config,path:b"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB:0\r\n";p.Server(("0.0.0.0",8081),None).serve_forever()' >/dev/null

docker run -d --name "$app" --network "$network" --network-alias application \
  --read-only --cap-drop ALL --security-opt no-new-privileges \
  --user "$(id -u):$(id -g)" --memory 3g --cpus 2 --pids-limit 256 \
  --tmpfs /tmp:rw,noexec,nosuid,size=128m --mount "type=volume,src=$volume,dst=/data,volume-nocopy" \
  -e ATO_BINDING_APP_ORIGIN=http://localhost:4200 -e REGISTRY_PREINSTALLED_EXTENSIONS=true \
  -e CACHE_DRIVER=redis -e QUEUE_CONNECTION=redis -e BROADCAST_DRIVER=socketcluster \
  -e MAIL_MAILER=log "$app_image" >/dev/null

docker run -d --name "$console" --network "$network" --network-alias console \
  --read-only --cap-drop ALL --security-opt no-new-privileges --user 65532:65532 \
  --tmpfs /tmp:rw,noexec,nosuid,size=64m \
  -e ATO_BINDING_APP_ORIGIN=http://localhost:4200 "$console_image" >/dev/null

docker run --rm --network "$network" --read-only --cap-drop ALL \
  --security-opt no-new-privileges --user 65532:65532 \
  --mount "type=bind,src=$here/socket_roundtrip.js,dst=/probe.js,readonly" \
  -e LD_LIBRARY_PATH=/opt/socket-libs --entrypoint /opt/fleetbase-socket/node \
  "$app_image" /probe.js
