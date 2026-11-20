#!/bin/sh
# Acceptance check: the Instance is resident (see residency.mjs), not merely
# "ready and verified". Run on the host as the Compose operator:
#   acceptance/residency.sh INSTANCE_ID
# Exit 0 only when resident; the JSON on stdout names every unmet condition.
# Run it twice a few minutes apart: authorization_generation must increase,
# which shows that the Runner is renewing the authorization.
set -eu
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
[ "$#" -eq 1 ] || {
  echo 'usage: residency.sh INSTANCE_ID' >&2
  exit 2
}
exec "$here/../stack" exec -T api node --no-warnings --input-type=module - "$1" < "$here/residency.mjs"
