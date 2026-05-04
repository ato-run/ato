#!/bin/bash
# Postgres provider bootstrap for P7 (since [provision] block execution
# is deferred from v1 MVP, we embed init-or-exec in this wrapper script).
#
# Args: $1 = state_dir, $2 = port, $3 = path to credential file
#
# RFC §7.3.2 Rule M1: the credential is materialized as a temp file with
# 0600 perms. We read the password ONCE from that file at init time
# (initdb --pwfile=<path>) and never again — postgres reuses the
# initialized cluster on subsequent starts. The password file is
# unlinked by the orchestrator at teardown.

set -eu

STATE_DIR="$1"
PORT="$2"
PWFILE="$3"

PGDATA="${STATE_DIR}/pgdata"

# Postgres Unix-domain socket path is bounded to 103 bytes on macOS. State
# dirs derived from <ato_home>/<parent>/<hash>/<state.version>/<state.name>/
# routinely exceed that. We disable Unix sockets entirely (TCP-only) by
# setting unix_socket_directories = '' on every postgres invocation.

PG_OPTS=(
  "-c" "listen_addresses=127.0.0.1"
  "-c" "unix_socket_directories="
)

if [ ! -f "${PGDATA}/PG_VERSION" ]; then
  echo "[ato/postgres bootstrap] initdb at ${PGDATA}" >&2
  /opt/homebrew/bin/initdb \
    -D "${PGDATA}" \
    --encoding=UTF8 \
    --username=postgres \
    --auth-local=password \
    --auth-host=password \
    --pwfile="${PWFILE}" \
    --no-instructions >&2
  if [ -n "${ATO_PG_DATABASE:-}" ]; then
    echo "[ato/postgres bootstrap] creating database ${ATO_PG_DATABASE}" >&2
    /opt/homebrew/bin/pg_ctl \
      -D "${PGDATA}" \
      -l "${PGDATA}/init.log" \
      -o "-p ${PORT} ${PG_OPTS[*]}" \
      -w start
    PGPASSWORD="$(cat "${PWFILE}")" \
      /opt/homebrew/bin/createdb \
      -h 127.0.0.1 -p "${PORT}" -U postgres \
      "${ATO_PG_DATABASE}"
    /opt/homebrew/bin/pg_ctl -D "${PGDATA}" -m fast stop
  fi
  echo "[ato/postgres bootstrap] init complete" >&2
fi

echo "[ato/postgres bootstrap] starting postgres on 127.0.0.1:${PORT}" >&2
exec /opt/homebrew/bin/postgres \
  -D "${PGDATA}" \
  -p "${PORT}" \
  "${PG_OPTS[@]}"
