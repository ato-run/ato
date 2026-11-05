#!/bin/sh
set -eu
: "${ATO_BINDING_INITIAL_ADMIN_PASSWORD:?Initial admin password is required}"
export ADMIN_PASSWORD="$ATO_BINDING_INITIAL_ADMIN_PASSWORD"
unset ATO_BINDING_INITIAL_ADMIN_PASSWORD
exec /usr/bin/dumb-init -- ./wait-for-db.sh node --max-http-header-size=32768 server.js
