#!/bin/bash
# Postgres provider bootstrap for P7 + WasedaP2P.
#
# Args: $1 = state_dir, $2 = port, $3 = path to credential file
#
# RFC §7.3.2 Rule M1: the credential is materialized as a temp file with
# 0600 perms. We read the password ONCE from that file at init time
# (initdb --pwfile=<path>) and never again — postgres reuses the
# initialized cluster on subsequent starts. The password file is
# unlinked by the orchestrator at teardown.
#
# Tool binaries come from ato-cli's verified tool artifact resolver
# (#119/#120). The capsule declares `tool_artifacts = ["postgresql"]`
# in capsule.toml; ato-cli downloads, verifies, and exposes the
# resolved paths via env. We require the env vars below — if any are
# missing, ato-cli is older than 0.5.x and the capsule cannot run on
# this host.

set -eu

STATE_DIR="$1"
PORT="$2"
PWFILE="$3"

require_tool_env() {
  local var="$1"
  local val="${!var:-}"
  if [ -z "$val" ]; then
    echo "[ato/postgres bootstrap] FATAL: $var is unset." >&2
    echo "[ato/postgres bootstrap] This capsule requires ato-cli >= 0.5.x with the tool artifact resolver." >&2
    echo "[ato/postgres bootstrap] Older ato-cli versions injected /opt/homebrew/bin/* directly; that path is removed." >&2
    exit 78  # EX_CONFIG
  fi
  if [ ! -x "$val" ]; then
    echo "[ato/postgres bootstrap] FATAL: $var=$val is not executable." >&2
    exit 78
  fi
}

require_tool_env ATO_TOOL_INITDB
require_tool_env ATO_TOOL_POSTGRES
require_tool_env ATO_TOOL_PG_CTL

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
  "${ATO_TOOL_INITDB}" \
    -D "${PGDATA}" \
    --encoding=UTF8 \
    --username=postgres \
    --auth-local=password \
    --auth-host=password \
    --pwfile="${PWFILE}" \
    --no-instructions >&2

  if [ -n "${ATO_PG_DATABASE:-}" ]; then
    # zonky's distribution does not ship `createdb` or `psql`. Use
    # postgres single-user mode (`--single`) to run the CREATE DATABASE
    # SQL directly against the catalog. Single-user mode is
    # network-less and authentication-less, intended exactly for
    # one-shot init like this.
    echo "[ato/postgres bootstrap] creating database ${ATO_PG_DATABASE} via postgres --single" >&2
    echo "CREATE DATABASE \"${ATO_PG_DATABASE}\";" | \
      "${ATO_TOOL_POSTGRES}" --single -D "${PGDATA}" postgres >&2
  fi
  echo "[ato/postgres bootstrap] init complete" >&2
fi

echo "[ato/postgres bootstrap] starting postgres on 127.0.0.1:${PORT}" >&2
exec "${ATO_TOOL_POSTGRES}" \
  -D "${PGDATA}" \
  -p "${PORT}" \
  "${PG_OPTS[@]}"
