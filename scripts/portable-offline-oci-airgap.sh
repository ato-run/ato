#!/usr/bin/env bash
# Prove that one offline portable OCI route starts from an empty, private
# Docker store in a Linux network namespace with no route to an external
# network. The shared host daemon and its image cache are never modified.

set -euo pipefail

usage() {
  printf '%s\n' \
    "usage: sudo $0 --ato PATH --bundle PATH --derivation SHA256_REF \\" \
    "  --work-root NEW_ABSOLUTE_PATH [--run-user USER]"
}

ATO_BIN=""
BUNDLE=""
DERIVATION=""
WORK_ROOT=""
RUN_USER="${SUDO_USER:-}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --ato)
      ATO_BIN="${2:-}"
      shift 2
      ;;
    --bundle)
      BUNDLE="${2:-}"
      shift 2
      ;;
    --derivation)
      DERIVATION="${2:-}"
      shift 2
      ;;
    --work-root)
      WORK_ROOT="${2:-}"
      shift 2
      ;;
    --run-user)
      RUN_USER="${2:-}"
      shift 2
      ;;
    *)
      usage >&2
      exit 64
      ;;
  esac
done

if [[ ${EUID} -ne 0 ]]; then
  printf 'portable air-gap acceptance requires root for a private network namespace\n' >&2
  exit 77
fi
if [[ -z "$ATO_BIN" || -z "$BUNDLE" || -z "$DERIVATION" || -z "$WORK_ROOT" || -z "$RUN_USER" ]]; then
  usage >&2
  exit 64
fi
if [[ ! -x "$ATO_BIN" || ! -r "$BUNDLE" ]]; then
  printf 'ato must be executable and bundle must be readable\n' >&2
  exit 66
fi
if [[ "$WORK_ROOT" != /* || "$WORK_ROOT" == "/" || "$WORK_ROOT" == "/home" ]]; then
  printf 'work root must be a new, specific absolute path\n' >&2
  exit 64
fi
if [[ ! "$DERIVATION" =~ ^sha256:[0-9a-f]{64}$ ]]; then
  printf 'derivation must be a lowercase SHA-256 reference\n' >&2
  exit 64
fi
id "$RUN_USER" >/dev/null

if [[ "${ATO_AIRGAP_INSIDE:-0}" != "1" ]]; then
  if [[ -e "$WORK_ROOT" ]]; then
    printf 'refusing to reuse work root: %s\n' "$WORK_ROOT" >&2
    exit 73
  fi
  # Keep the host mount namespace shared with containerd/runc. Only the
  # network namespace is part of this acceptance boundary; isolating mounts
  # here would hide Docker's overlay rootfs from the host container runtime.
  exec unshare --net \
    env ATO_AIRGAP_INSIDE=1 "$0" \
      --ato "$ATO_BIN" \
      --bundle "$BUNDLE" \
      --derivation "$DERIVATION" \
      --work-root "$WORK_ROOT" \
      --run-user "$RUN_USER"
fi

RUN_GROUP="$(id -gn "$RUN_USER")"
RUN_HOME="$(getent passwd "$RUN_USER" | cut -d: -f6)"
SOCKET="$WORK_ROOT/docker.sock"
RECEIPT="$WORK_ROOT/receipt.json"
DOCKER_LOG="$WORK_ROOT/dockerd.log"

install -d -m 0750 -o "$RUN_USER" -g "$RUN_GROUP" "$WORK_ROOT"
install -d -m 0710 "$WORK_ROOT/docker-data" "$WORK_ROOT/docker-exec"
ip link set lo up

{
  printf 'kernel=%s\n' "$(uname -srmo)"
  printf 'run_user=%s\n' "$RUN_USER"
  printf 'bundle_sha256=sha256:%s\n' "$(sha256sum "$BUNDLE" | cut -d' ' -f1)"
  printf 'network_before_dockerd:\n'
  ip -brief address
  printf 'routes_before_dockerd:\n'
  ip route show
} >"$WORK_ROOT/namespace-before.txt"

if [[ -n "$(ip route show default)" ]]; then
  printf 'private namespace unexpectedly has a default route\n' >&2
  exit 1
fi
if [[ "$(ip -o link show | awk -F': ' '{sub(/@.*/, "", $2); print $2}' | sort)" != "lo" ]]; then
  printf 'private namespace unexpectedly inherited a non-loopback interface\n' >&2
  exit 1
fi

dockerd \
  --host "unix://$SOCKET" \
  --data-root "$WORK_ROOT/docker-data" \
  --exec-root "$WORK_ROOT/docker-exec" \
  --pidfile "$WORK_ROOT/dockerd.pid" \
  --feature containerd-snapshotter=false \
  --group "$RUN_GROUP" \
  --log-level info >"$DOCKER_LOG" 2>&1 &
DOCKERD_PID=$!

stop_dockerd() {
  if kill -0 "$DOCKERD_PID" 2>/dev/null; then
    mapfile -t leftover_containers < <(docker --host "unix://$SOCKET" ps -aq 2>/dev/null || true)
    if [[ ${#leftover_containers[@]} -gt 0 ]]; then
      docker --host "unix://$SOCKET" rm --force "${leftover_containers[@]}" >/dev/null 2>&1 || true
    fi
    kill -TERM "$DOCKERD_PID" 2>/dev/null || true
    wait "$DOCKERD_PID" 2>/dev/null || true
  fi
}
trap stop_dockerd EXIT INT TERM

for _ in $(seq 1 120); do
  if docker --host "unix://$SOCKET" info >/dev/null 2>&1; then
    break
  fi
  if ! kill -0 "$DOCKERD_PID" 2>/dev/null; then
    printf 'private dockerd exited before becoming ready\n' >&2
    tail -80 "$DOCKER_LOG" >&2
    exit 1
  fi
  sleep 0.25
done
docker --host "unix://$SOCKET" info >/dev/null

INITIAL_IMAGES="$(docker --host "unix://$SOCKET" image ls -q)"
if [[ -n "$INITIAL_IMAGES" ]]; then
  printf 'private Docker store is not empty before the test\n' >&2
  exit 1
fi
if [[ -n "$(ip route show default)" ]]; then
  printf 'dockerd unexpectedly installed an external default route\n' >&2
  exit 1
fi

{
  printf 'docker_version=%s\n' "$(docker --host "unix://$SOCKET" version --format '{{.Server.Version}}')"
  printf 'initial_image_count=0\n'
  printf 'network_after_dockerd:\n'
  ip -brief address
  printf 'routes_after_dockerd:\n'
  ip route show
} >"$WORK_ROOT/namespace-ready.txt"

set +e
runuser -u "$RUN_USER" -- env \
  -u HTTP_PROXY -u HTTPS_PROXY -u ALL_PROXY \
  -u http_proxy -u https_proxy -u all_proxy \
  HOME="$RUN_HOME" \
  DOCKER_HOST="unix://$SOCKET" \
  "$ATO_BIN" run "$BUNDLE" \
    --derivation "$DERIVATION" \
    --no-open \
    --verification-receipt "$RECEIPT"
RUN_STATUS=$?
set -e

if [[ $RUN_STATUS -ne 0 ]]; then
  docker --host "unix://$SOCKET" image ls --all --digests --no-trunc \
    >"$WORK_ROOT/failure-images.txt" 2>&1 || true
  mapfile -t image_ids < <(docker --host "unix://$SOCKET" image ls -q --no-trunc 2>/dev/null | sort -u)
  if [[ ${#image_ids[@]} -gt 0 ]]; then
    docker --host "unix://$SOCKET" image inspect "${image_ids[@]}" \
      >"$WORK_ROOT/failure-image-inspect.json" 2>&1 || true
  fi
  docker --host "unix://$SOCKET" ps --all --no-trunc \
    >"$WORK_ROOT/failure-containers.txt" 2>&1 || true
  mapfile -t failed_containers < <(docker --host "unix://$SOCKET" ps -aq 2>/dev/null)
  if [[ ${#failed_containers[@]} -gt 0 ]]; then
    docker --host "unix://$SOCKET" inspect "${failed_containers[@]}" \
      >"$WORK_ROOT/failure-container-inspect.json" 2>&1 || true
  fi
  exit "$RUN_STATUS"
fi

jq -e \
  --arg derivation "$DERIVATION" \
  '.fully_satisfied == true
   and .derivation_ref == $derivation
   and .execution.portability_profile == "offline"
   and (.execution.embedded_oci_image_loaded | type == "string")
   and ([.observations[].outcome] | all(. == "satisfied"))' \
  "$RECEIPT" >/dev/null

if [[ -n "$(docker --host "unix://$SOCKET" ps -aq)" ]]; then
  printf 'portable run left a container in the private Docker store\n' >&2
  exit 1
fi

{
  printf 'final_image_count=%s\n' "$(docker --host "unix://$SOCKET" image ls -q | sort -u | wc -l)"
  printf 'remaining_container_count=0\n'
  jq '{bundle_sha256,contract_ref,derivation_ref,fully_satisfied,execution,observations}' "$RECEIPT"
} >"$WORK_ROOT/result.txt"

printf 'offline OCI air-gap acceptance passed\n'
printf 'work_root=%s\n' "$WORK_ROOT"
printf 'receipt=%s\n' "$RECEIPT"
