#!/usr/bin/env bash
set -euo pipefail
umask 077
state_dir="$HOME/.local/share/ato-selfhost"
credentials="$state_dir/identity/runner.json"
if [[ -e "$credentials" ]]; then
  echo "This host already has a Runner identity at $credentials" >&2
  exit 1
fi
mkdir -p -- "$state_dir"/{work,volumes,identity}
# The Runner operator creates this private file locally. Use literal absolute paths
# and environment assignments supported by both systemd and the shell.
set -a
# shellcheck disable=SC1091
source "$HOME/.config/ato-selfhost/runner.env"
set +a
: "${ATO_API_URL:?Set the local Coordinator URL in runner.env}"
case "$ATO_API_URL" in
  https://*) ;;
  *) echo 'Runner enrollment requires an HTTPS API origin' >&2; exit 1 ;;
esac
read -r -s -p 'One-time Runner enrollment code: ' ATO_RUNNER_ENROLLMENT_TOKEN
echo >&2
: "${ATO_RUNNER_ENROLLMENT_TOKEN:?Empty enrollment code}"
export ATO_RUNNER_ENROLLMENT_TOKEN
exec "$HOME/.cargo/bin/ato-connected-realization-worker" \
  --work-root "$state_dir/work" \
  --state-volume-root "$state_dir/volumes" \
  --runner-credentials-file "$credentials" \
  --require-explicit-runtime-placement \
  --surface-target 127.0.0.1:18421 --once
