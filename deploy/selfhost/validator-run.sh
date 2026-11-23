#!/bin/bash
# Entrypoint of the validator container (mounted read-only by compose.yaml).
#
# The validator shares the API container's network namespace and reaches the
# verification listener on loopback. When Docker restarts the API container,
# that namespace is replaced and this container keeps the old, empty one: the
# validator then polls a dead address forever while still looking "running".
# This wrapper ends the container once the listener has been unreachable for
# VALIDATOR_API_GRACE_SECS, so the restart policy starts it again inside the
# API's current namespace. `./stack` handles the case in which the API
# container itself was replaced.
set -u
listener_host=127.0.0.1
listener_port=${VALIDATOR_PORT:-8788}
grace=${VALIDATOR_API_GRACE_SECS:-30}
interval=5

reachable() {
  (exec 3<>"/dev/tcp/$listener_host/$listener_port") 2>/dev/null
}

if [ "${1:-}" = probe ]; then
  reachable
  exit
fi

CAPSULE_VALIDATOR_AGENT_TOKEN=$(cat /run/secrets/validator_token) || exit 1
export CAPSULE_VALIDATOR_AGENT_TOKEN
ato-portable-validator "$@" &
worker=$!
trap 'kill -TERM "$worker" 2>/dev/null; wait "$worker"; exit 0' TERM INT

unreachable=0
while kill -0 "$worker" 2>/dev/null; do
  if reachable; then
    unreachable=0
  else
    unreachable=$((unreachable + interval))
    if [ "$unreachable" -ge "$grace" ]; then
      echo "validator: no API listener on $listener_host:$listener_port for ${unreachable}s; exiting to rejoin the API network namespace" >&2
      kill -TERM "$worker" 2>/dev/null
      wait "$worker"
      exit 70
    fi
  fi
  sleep "$interval" &
  wait $!
done
wait "$worker"
