#!/bin/sh
set -eu
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
tag=$(python3 -c 'import secrets; print(secrets.token_hex(6))')
network="fleetbase-proxy-proof-$tag"
backend="fleetbase-proxy-proof-backend-$tag"
http_console="fleetbase-proxy-proof-http-$tag"
https_console="fleetbase-proxy-proof-https-$tag"
https_default_console="fleetbase-proxy-proof-https-default-$tag"
app_image=${FLEETBASE_SOCKET_PROOF_APP_IMAGE:-ato-local-proof/fleetbase-combined:socket-roundtrip}
console_image=${FLEETBASE_SOCKET_PROOF_CONSOLE_IMAGE:-ato-local-proof/fleetbase-console:origin-bound-arm64}
cleanup() {
  docker stop --timeout 2 "$http_console" "$https_console" "$https_default_console" "$backend" >/dev/null 2>&1 || true
  docker rm -v "$http_console" "$https_console" "$https_default_console" "$backend" >/dev/null 2>&1 || true
  docker network rm "$network" >/dev/null 2>&1 || true
}
trap cleanup EXIT HUP INT TERM

for origin in '' 'http://outside.invalid' 'https://tenant.invalid:443' \
  'https://user@tenant.invalid' 'https://tenant.invalid/path' \
  'https://tenant.invalid;bad' 'https://tenant.invalid:65536' 'https://BAD.invalid' \
  'https://tenant.invalid//' 'https://-bad.invalid' 'https://tenant.invalid.' \
  'http://127.0.0.999' 'http://localhost:80' 'http://localhost:004200'; do
  output=$(docker run --rm --network none --read-only --user 65532:65532 \
    -e "ATO_BINDING_APP_ORIGIN=$origin" "$console_image" 2>&1) && {
      echo 'Invalid origin was accepted' >&2; exit 1;
    }
  case "$output" in *'Invalid or missing application origin'*) ;; *)
    echo 'Origin validation failed for an unexpected reason' >&2; exit 1 ;; esac
done
docker network create --internal "$network" >/dev/null
docker run -d --name "$backend" --network "$network" --network-alias application \
  --read-only --cap-drop ALL --security-opt no-new-privileges --user 65532:65532 \
  --mount "type=bind,src=$here/proxy_probe.js,dst=/probe.js,readonly" \
  -e LD_LIBRARY_PATH=/opt/socket-libs --entrypoint /opt/fleetbase-socket/node \
  "$app_image" /probe.js server >/dev/null
for kind in http https https-default; do
  case "$kind" in
    http) name=$http_console; origin=http://localhost:4200 ;;
    https) name=$https_console; origin=https://tenant.synthetic.invalid:8443 ;;
    https-default) name=$https_default_console; origin=https://tenant.synthetic.invalid ;;
  esac
  docker run -d --name "$name" --network "$network" --network-alias "console-$kind" \
    --read-only --cap-drop ALL --security-opt no-new-privileges --user 65532:65532 \
    --tmpfs /tmp:rw,noexec,nosuid,size=64m \
    -e "ATO_BINDING_APP_ORIGIN=$origin" "$console_image" >/dev/null
done
docker run --rm --network "$network" --read-only --cap-drop ALL \
  --security-opt no-new-privileges --user 65532:65532 \
  --mount "type=bind,src=$here/proxy_probe.js,dst=/probe.js,readonly" \
  -e LD_LIBRARY_PATH=/opt/socket-libs --entrypoint /opt/fleetbase-socket/node \
  "$app_image" /probe.js verify
