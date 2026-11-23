#!/bin/sh
set -eu
site=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
env_file="$site/.env"

# Read one value from .env without executing it. Surrounding quotes and an
# unquoted trailing comment are removed; the last assignment wins, as in
# Compose. The value is written only to command substitution, never printed.
env_value() {
  [ -f "$env_file" ] || return 1
  awk -v key="$1" '
    /^[[:space:]]*#/ { next }
    {
      line = $0
      sub(/\r$/, "", line)
      sub(/^[[:space:]]*/, "", line)
      equal = index(line, "=")
      if (equal == 0) next
      name = substr(line, 1, equal - 1)
      sub(/[[:space:]]+$/, "", name)
      if (name != key) next
      value = substr(line, equal + 1)
      sub(/^[[:space:]]+/, "", value)
      first = substr(value, 1, 1)
      if (first == "\"" || first == "\047") {
        quote = first
        rest = substr(value, 2)
        end_quote = index(rest, quote)
        if (end_quote > 0) value = substr(rest, 1, end_quote - 1)
        else value = substr(value, 2)
      } else {
        sub(/[[:space:]]+#.*$/, "", value)
        sub(/[[:space:]]+$/, "", value)
      }
      found = value
      seen = 1
    }
    END {
      if (!seen) exit 1
      print found
    }
  ' "$env_file"
}

auth_methods=
if ! auth_methods=$(env_value ATO_AUTH_METHODS); then
  auth_methods=
fi
trusted_proxy=false
case ",$(printf '%s' "$auth_methods" | tr -d '[:space:]')," in
  *,trusted_proxy,*) trusted_proxy=true ;;
esac

verify=
if ! verify=$(env_value ATO_TRUSTED_PROXY_VERIFY); then
  verify=
fi
if [ "$trusted_proxy" = true ] && [ "$verify" = header ]; then
  echo 'Refusing to start: trusted_proxy with header verification is not allowed.' >&2
  exit 1
fi

if ! configured_services=$("$site/stack" config --services); then
  echo 'Could not read the configured Compose services.' >&2
  exit 1
fi
tailscale_configured=false
while IFS= read -r configured_service; do
  if [ "$configured_service" = tailscale ]; then
    tailscale_configured=true
  fi
done <<SERVICES_EOF
$configured_services
SERVICES_EOF

if [ "$trusted_proxy" = true ] && [ "$verify" = tailscale_whois ] &&
  [ "$tailscale_configured" != true ]; then
  echo 'Refusing to start: trusted_proxy with tailscale_whois requires the tailscale service (set COMPOSE_PROFILES=tailscale in .env).' >&2
  exit 1
fi

"$site/init-runtime.sh"
# Replace the API first: ./stack then moves the validator into the new API
# network namespace before both are awaited as healthy.
start_api() {
  "$site/stack" up -d --no-build --pull never api
  "$site/stack" up -d --wait --wait-timeout 90 --no-build --pull never api validator
}
"$site/stack" up -d --no-build --pull never web
if [ "$tailscale_configured" = true ]; then
  "$site/stack" up -d --no-build --pull never tailscale
  "$site/stack" up -d --no-build --pull never whois
fi
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
