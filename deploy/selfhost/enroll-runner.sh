#!/usr/bin/env bash
set -euo pipefail
umask 077

# Runner 0 is the installation this distribution has always enrolled: its own
# env file, state directory and surface target. Runner 1..3 are extra
# single-slot Runners on the same guest, each enrolled with its own identity.
case "${1:-}" in
  "") runner=0 ;;
  1 | 2 | 3) runner=$1 ;;
  *)
    echo 'Usage: enroll-runner.sh [runner-number 1|2|3]' >&2
    exit 1
    ;;
esac
if [[ $# -gt 1 ]]; then
  echo 'Usage: enroll-runner.sh [runner-number 1|2|3]' >&2
  exit 1
fi

if [[ "$runner" = 0 ]]; then
  state_dir="$HOME/.local/share/ato-selfhost"
  env_file="$HOME/.config/ato-selfhost/runner.env"
  surface_target=127.0.0.1:18421
else
  state_dir="$HOME/.local/share/ato-selfhost/runners/$runner"
  env_file="$HOME/.config/ato-selfhost/runner-$runner.env"
  surface_target="127.0.0.1:$((18440 + runner))"
fi
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
source "$env_file"
set +a
: "${ATO_API_URL:?Set the local Coordinator URL in ${env_file##*/}}"
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
  --surface-target "$surface_target" --once
