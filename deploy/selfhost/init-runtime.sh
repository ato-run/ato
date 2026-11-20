#!/bin/sh
set -eu
site=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
keys="$site/.tmp/secrets"
mkdir -p "$keys" "$site/.tmp/trust" "$site/.tmp/runner-ingress"
chmod 0700 "$keys"
chmod 0755 "$site/.tmp/trust"
if [ ! -f "$keys/validator_token" ]; then
  umask 077
  temp=$(mktemp "$keys/.validator.XXXXXXXX")
  trap 'rm -f -- "$temp"' EXIT
  trap 'exit 1' INT TERM HUP
  openssl rand -hex 32 > "$temp"
  # Compose mounts secret files verbatim. Only the two intended services
  # receive this file; the host parent is accessible only by the operator.
  chmod 0444 "$temp"
  ln "$temp" "$keys/validator_token" || test -s "$keys/validator_token"
fi
test -s "$keys/validator_token"
