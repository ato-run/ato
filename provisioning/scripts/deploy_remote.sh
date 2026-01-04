#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
DEPLOY_PY="${SCRIPT_DIR}/deploy.py"

usage() {
  cat <<'EOF'
Usage: deploy_remote.sh --host <user@host> [options]

Copy deploy.py to a remote rig node (e.g. OCI A1 instance) and execute it via SSH.
Requires password-less SSH (public key) access or an identity file.

Options:
  -h, --host <user@host>        SSH destination in user@hostname format (required)
  -i, --identity <path>         SSH identity (private key) to use
  -p, --port <port>             SSH port (defaults to 22)
      --remote-dir <path>       Remote directory to place scripts (default: /tmp/capsuled-deploy-<timestamp>)
      --python <command>        Python executable to run deploy.py (default: python3)
      --auth-key <key>          TAILSCALE_AUTHKEY value to export remotely
      --hostname <name>         TAILSCALE_HOSTNAME value to export remotely
      --login-server <url>      TAILSCALE_LOGIN_SERVER value to export remotely
      --ssh-option <arg>        Additional ssh/scp option (repeatable)
      --verbose                 Enable verbose SSH/SCP output and unbuffered remote logs
  -?, --help                    Show this help message

Examples:
  ./deploy_remote.sh --host ubuntu@203.0.113.10 --identity ~/.ssh/rig.pem --auth-key tskey-abc123
  ./deploy_remote.sh --host ubuntu@rig --remote-dir /home/ubuntu/onescluster-deploy
EOF
}

HOST=""
IDENTITY=""
PORT=""
PYTHON_CMD="python3"
REMOTE_DIR=""
AUTH_KEY=""
TS_HOSTNAME=""
LOGIN_SERVER=""
EXTRA_SSH_OPTS=()
VERBOSE=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--host)
      HOST="$2"; shift 2 ;;
    -i|--identity)
      IDENTITY="$2"; shift 2 ;;
    -p|--port)
      PORT="$2"; shift 2 ;;
    --python)
      PYTHON_CMD="$2"; shift 2 ;;
    --remote-dir)
      REMOTE_DIR="$2"; shift 2 ;;
    --auth-key)
      AUTH_KEY="$2"; shift 2 ;;
    --hostname)
      TS_HOSTNAME="$2"; shift 2 ;;
    --login-server)
      LOGIN_SERVER="$2"; shift 2 ;;
    --ssh-option)
      EXTRA_SSH_OPTS+=("$2"); shift 2 ;;
    --verbose)
      VERBOSE=1; shift ;;
    -?|--help)
      usage; exit 0 ;;
    *)
      echo "Unknown argument: $1" >&2
      usage
      exit 1 ;;
  esac
done

if [[ -z "${HOST}" ]]; then
  echo "Error: --host is required" >&2
  usage
  exit 1
fi

if [[ ${VERBOSE} -eq 1 ]]; then
  set -x
fi

if [[ ! -f "${DEPLOY_PY}" ]]; then
  echo "Error: deploy.py not found at ${DEPLOY_PY}" >&2
  exit 1
fi

if [[ -z "${REMOTE_DIR}" ]]; then
  REMOTE_DIR="/tmp/capsuled-deploy-$(date +%s)"
fi

SSH_OPTS=(-o BatchMode=yes)
if [[ -n "${IDENTITY}" ]]; then
  SSH_OPTS+=( -i "${IDENTITY}" )
fi
if [[ -n "${PORT}" ]]; then
  SSH_OPTS+=( -p "${PORT}" )
fi
if [[ ${#EXTRA_SSH_OPTS[@]} -gt 0 ]]; then
  SSH_OPTS+=( "${EXTRA_SSH_OPTS[@]}" )
fi
if [[ ${VERBOSE} -eq 1 ]]; then
  SSH_OPTS+=( -v )
fi

printf '==> Creating remote directory %s\n' "${REMOTE_DIR}"
ssh "${SSH_OPTS[@]}" "${HOST}" "mkdir -p $(printf '%q' "${REMOTE_DIR}")"

printf '==> Copying deploy.py to remote host\n'
scp "${SSH_OPTS[@]}" "${DEPLOY_PY}" "${HOST}:$(printf '%q' "${REMOTE_DIR}")/deploy.py"

declare -a ENV_ASSIGNMENTS=()
if [[ -n "${AUTH_KEY}" ]]; then
  ENV_ASSIGNMENTS+=("TAILSCALE_AUTHKEY=$(printf '%q' "${AUTH_KEY}")")
fi
if [[ -n "${TS_HOSTNAME}" ]]; then
  ENV_ASSIGNMENTS+=("TAILSCALE_HOSTNAME=$(printf '%q' "${TS_HOSTNAME}")")
fi
if [[ -n "${LOGIN_SERVER}" ]]; then
  ENV_ASSIGNMENTS+=("TAILSCALE_LOGIN_SERVER=$(printf '%q' "${LOGIN_SERVER}")")
fi
if [[ ${VERBOSE} -eq 1 ]]; then
  ENV_ASSIGNMENTS=("PYTHONUNBUFFERED=1" "${ENV_ASSIGNMENTS[@]}")
fi

REMOTE_CMD="cd $(printf '%q' "${REMOTE_DIR}") && ${PYTHON_CMD} deploy.py"
if [[ ${#ENV_ASSIGNMENTS[@]} -gt 0 ]]; then
  REMOTE_CMD="cd $(printf '%q' "${REMOTE_DIR}") && env ${ENV_ASSIGNMENTS[*]} ${PYTHON_CMD} deploy.py"
fi

printf '==> Executing deploy.py on %s\n' "${HOST}"
ssh "${SSH_OPTS[@]}" "${HOST}" "${REMOTE_CMD}"

printf '\n✅ Remote deployment finished. Review the output above for details.\n'
