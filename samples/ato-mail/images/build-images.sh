#!/usr/bin/env bash
# Build the two images the D1 sample pins.
#
#   ./build-images.sh <Govcraft/missive checkout> [platform]
#
# The checkout must be at 7716106b6c4638426169dac264657437914bbe7f and contain
# package.json, Cargo.toml/Cargo.lock, src/ and templates/. The script copies
# this directory's patches into <checkout>/ato-patches/ as Dockerfile.missive
# expects, builds both images, and prints the resulting digests.
set -euo pipefail

missive_src=${1:?usage: build-images.sh <Govcraft/missive checkout> [platform]}
platform=${2:-linux/amd64}
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

for required in package.json Cargo.toml Cargo.lock src templates; do
  test -e "${missive_src}/${required}" || {
    echo "error: ${missive_src} does not look like the Govcraft/missive checkout (missing ${required})" >&2
    exit 1
  }
done

mkdir -p "${missive_src}/ato-patches"
cp "${here}/missive-nondefault-port.patch" \
   "${here}/missive-identity-submission-capability.patch" \
   "${missive_src}/ato-patches/"

echo "== Stalwart thin image (${platform})"
docker buildx build --platform "${platform}" --load \
  -f "${here}/Dockerfile.stalwart" \
  -t ato-mail/stalwart:local "${here}"

echo "== Missive patched image (${platform})"
docker buildx build --platform "${platform}" --load \
  -f "${here}/Dockerfile.missive" \
  -t ato-mail/missive:local "${missive_src}"

for image in ato-mail/stalwart:local ato-mail/missive:local; do
  echo "== ${image}"
  docker image inspect "${image}" \
    --format 'platform={{.Os}}/{{.Architecture}} user={{.Config.User}} entrypoint={{json .Config.Entrypoint}} digests={{json .RepoDigests}}'
done

cat <<'NOTE'
NOTE

echo
echo "Next: pin the staging-tag digests in samples/ato-mail/images/images.lock and"
echo "capsule.toml, then preload the images on the dedicated Runner (see README.md)."
