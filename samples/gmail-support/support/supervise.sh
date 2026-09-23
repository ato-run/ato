#!/bin/sh
set -eu

for name in \
  ATO_BINDING_CHATWOOT_SECRET_KEY \
  ATO_BINDING_CHATWOOT_RUNTIME \
  ATO_BINDING_GOOGLE_OAUTH_CLIENT \
  ATO_BINDING_GMAIL_OAUTH \
  ATO_BINDING_APP_ORIGIN; do
  eval "value=\${$name:-}"
  if [ -z "$value" ]; then
    echo "required Support Desk Binding is absent: $name" >&2
    exit 64
  fi
done

state_root=/var/lib/ato-support
export PGDATA="$state_root/postgres"
mkdir -p "$PGDATA" "$state_root/storage" "$state_root/bridge" /run/ato-support /run/postgresql
chown -R 1000:1000 "$state_root" /run/ato-support /run/postgresql
chmod 0700 "$PGDATA" "$state_root/bridge" /run/ato-support

if [ ! -s "$PGDATA/PG_VERSION" ]; then
  # PostgreSQL's default 16 MiB WAL segments make a fresh Chatwoot state exceed
  # the current 64 MiB filesystem-state transport before it contains any mail.
  # The supported 1 MiB segment size only changes WAL rotation granularity.
  su-exec ato /usr/bin/initdb \
    --username=postgres \
    --auth-local=trust \
    --auth-host=trust \
    --encoding=UTF8 \
    --wal-segsize=1
fi
su-exec ato /usr/bin/pg_ctl -D "$PGDATA" \
  -o "-c listen_addresses=127.0.0.1 -c min_wal_size=5MB -c max_wal_size=16MB" \
  -w start

export SECRET_KEY_BASE="$ATO_BINDING_CHATWOOT_SECRET_KEY"
export RAILS_ENV=production
export NODE_ENV=production
export INSTALLATION_ENV=docker
export POSTGRES_HOST=127.0.0.1
export POSTGRES_PORT=5432
export POSTGRES_DATABASE=chatwoot
export POSTGRES_USERNAME=postgres
export REDIS_URL=redis://redis:6379
export ACTIVE_STORAGE_SERVICE=local
export ENABLE_ACCOUNT_SIGNUP=false
export DISABLE_TELEMETRY=true
export FORCE_SSL=false
export FRONTEND_URL="$ATO_BINDING_APP_ORIGIN"
export RAILS_LOG_TO_STDOUT=true
export RAILS_SERVE_STATIC_FILES=true

attempt=0
until ruby -rsocket -e 'socket = TCPSocket.new("redis", 6379); socket.close'; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 30 ]; then
    echo "Redis did not become ready" >&2
    exit 1
  fi
  sleep 1
done

cd /app
attempt=0
until su-exec ato bundle exec rails db:chatwoot_prepare; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 30 ]; then
    echo "Chatwoot database preparation did not become ready" >&2
    exit 1
  fi
  sleep 2
done

# `initdb` creates a maintenance database in addition to template0/template1.
# Chatwoot uses its own database after preparation, so retaining that extra
# clone needlessly consumes several MiB of the bounded filesystem state.
su-exec ato dropdb \
  --host=127.0.0.1 \
  --username=postgres \
  --maintenance-db=template1 \
  --if-exists postgres

su-exec ato bundle exec rails runner /opt/gmail-support/bootstrap_chatwoot.rb
ATO_BINDING_CHATWOOT_RUNTIME=$(cat /run/ato-support/chatwoot-runtime.json)
export ATO_BINDING_CHATWOOT_RUNTIME
export BRIDGE_DATABASE="$state_root/bridge/bridge.sqlite3"
export CHATWOOT_URL=http://127.0.0.1:3000

su-exec ato bundle exec rails server -p 3000 -b 127.0.0.1 &
web_pid=$!
su-exec ato bundle exec sidekiq -C config/sidekiq.yml &
worker_pid=$!
su-exec ato /opt/gmail-support/venv/bin/python -m gmail_support.app &
bridge_pid=$!

cleanup() {
  trap - INT TERM EXIT
  kill -TERM "$web_pid" "$worker_pid" "$bridge_pid" 2>/dev/null || true
  wait "$web_pid" "$worker_pid" "$bridge_pid" 2>/dev/null || true
  su-exec ato /usr/bin/pg_ctl -D "$PGDATA" -m fast -w stop 2>/dev/null || true
}

shutdown() {
  cleanup
  exit 0
}

trap shutdown INT TERM
trap cleanup EXIT

while kill -0 "$web_pid" 2>/dev/null \
   && kill -0 "$worker_pid" 2>/dev/null \
   && kill -0 "$bridge_pid" 2>/dev/null \
   && su-exec ato /usr/bin/pg_ctl -D "$PGDATA" status >/dev/null 2>&1; do
  sleep 1
done

echo "a Support Desk process exited; stopping the service" >&2
exit 1
