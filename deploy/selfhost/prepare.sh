#!/bin/sh
set -eu
umask 077
site=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo=$(git -C "$site" rev-parse --show-toplevel)
# shellcheck disable=SC1091
unset ATO_VALIDATOR_REV
. "$site/versions.env"
mkdir -p "$site/.tmp/src"
stage=$(mktemp -d "$site/.tmp/prepare.XXXXXXXX")
trap 'rm -rf -- "$stage"' EXIT
trap 'exit 1' INT TERM HUP

extract() {
  checkout=$1
  revision=$2
  if [ "$(git -C "$checkout" rev-parse --verify "$revision^{commit}")" != "$revision" ]; then
    echo "Missing pinned commit $revision in $checkout" >&2
    exit 1
  fi
  dest="$site/.tmp/src/$revision"
  if [ -d "$dest" ]; then
    return
  fi
  mkdir "$stage/$revision"
  git -C "$checkout" archive --output="$stage/source.tar" "$revision"
  tar -xf "$stage/source.tar" -C "$stage/$revision"
  mv "$stage/$revision" "$dest"
}

extract "${1:-$repo/../ato-api}" "$ATO_API_REV"
extract "${2:-$repo/../ato-pwa}" "$ATO_PWA_REV"
extract "$repo" "$ATO_RUNNER_REV"
extract "$repo" "${ATO_VALIDATOR_REV:-$ATO_RUNNER_REV}"
"$site/init-runtime.sh"
echo "Prepared the pinned API, PWA and Runner sources in $site/.tmp/src"
