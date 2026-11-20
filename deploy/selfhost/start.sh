#!/bin/sh
set -eu
site=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
"$site/init-runtime.sh"
# Replace the API first: ./stack then moves the validator into the new API
# network namespace before both are awaited as healthy.
start_api() {
  "$site/stack" up -d --no-build --pull never api
  "$site/stack" up -d --wait --wait-timeout 90 --no-build --pull never api validator
}
"$site/stack" up -d --no-build --pull never web
if [ -s "$site/.tmp/tls/fullchain.pem" ] && [ -s "$site/.tmp/tls/privkey.pem" ]; then
  # Publicly trusted certificate: there is no internal CA to distribute.
  rm -f -- "$site/.tmp/trust/root.crt"
  start_api
  exit 0
fi
# Install only the public CA for the API's TLS connection to the Runner gate.
# Keep the CA private key inside Caddy's own volume.
incoming=$(mktemp "$site/.tmp/trust/.root.XXXXXXXX")
trap 'rm -f -- "$incoming"' EXIT
trap 'exit 1' INT TERM HUP
tries=0
until "$site/stack" exec -T web cat /data/caddy/pki/authorities/local/root.crt > "$incoming"; do
  tries=$((tries + 1))
  if [ "$tries" -ge 30 ]; then
    echo 'Could not read the Caddy public CA. Check ./stack logs web.' >&2
    exit 1
  fi
  sleep 1
done
openssl x509 -in "$incoming" -noout
if [ -f "$site/.tmp/trust/root.crt" ] && ! cmp -s "$incoming" "$site/.tmp/trust/root.crt"; then
  echo 'The Caddy CA changed. Verify the replacement or restore the original tls-data volume.' >&2
  exit 1
fi
chmod 0644 "$incoming"
mv "$incoming" "$site/.tmp/trust/root.crt"
start_api
