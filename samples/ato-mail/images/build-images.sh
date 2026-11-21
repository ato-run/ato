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

# The Dockerfile applies the patches itself, in this order, so the checkout must
# be the unpatched pinned commit. Check the whole stack on a scratch copy of the
# source: each patch is applied on top of the previous ones, and the copy is
# discarded, so a stale or already-patched checkout is caught without touching
# it. (All four patches only touch src/.)
patch_check="${missive_src}/.ato-patch-check.$$"
trap 'rm -rf "${patch_check}"' EXIT
rm -rf "${patch_check}"
mkdir -p "${patch_check}"
cp -R "${missive_src}/src" "${patch_check}/src"
for patch in missive-nondefault-port.patch \
             missive-identity-submission-capability.patch \
             missive-ato-identity.patch \
             missive-ato-accounts.patch; do
  if ! ( cd "${patch_check}" && patch -p1 --forward < "${here}/${patch}" >/dev/null 2>&1 ); then
    echo "error: ${patch} does not apply cleanly on top of the earlier patches in ${missive_src}." >&2
    echo "       Use a clean checkout at 7716106b6c4638426169dac264657437914bbe7f." >&2
    exit 1
  fi
done
rm -rf "${patch_check}"

mkdir -p "${missive_src}/ato-patches"
cp "${here}/missive-nondefault-port.patch" \
   "${here}/missive-identity-submission-capability.patch" \
   "${here}/missive-ato-identity.patch" \
   "${here}/missive-ato-accounts.patch" \
   "${missive_src}/ato-patches/"

build_image() {
  local name=$1 context=$2 dockerfile=$3
  if docker buildx version >/dev/null 2>&1; then
    docker buildx build --platform "${platform}" --load \
      -f "${dockerfile}" -t "ato-mail/${name}:local" "${context}"
  else
    echo "note: docker buildx is unavailable; using docker build." >&2
    echo "      The Dockerfiles pin linux/amd64 base manifests, so this still" >&2
    echo "      produces the staging platform (possibly through emulation)." >&2
    docker build -f "${dockerfile}" -t "ato-mail/${name}:local" "${context}"
  fi
}

echo "== Stalwart thin image"
build_image stalwart "${here}" "${here}/Dockerfile.stalwart"

echo "== Missive patched image"
build_image missive "${missive_src}" "${here}/Dockerfile.missive"

# The Runner refuses an image-declared VOLUME without a mount at that exact path,
# and upstream Stalwart declares /etc/stalwart. Flattening drops the declaration.
echo "== Stalwart flat image"
stalwart_container=$(docker create --platform "${platform}" ato-mail/stalwart:local --version)
docker export "${stalwart_container}" | docker import --platform "${platform}" \
  --change 'USER 1000:1000' --change 'ENTRYPOINT ["/usr/local/bin/stalwart"]' \
  - ato-mail/stalwart:flat >/dev/null
docker rm "${stalwart_container}" >/dev/null

for image in ato-mail/stalwart:flat ato-mail/missive:local; do
  echo "== ${image}"
  docker image inspect "${image}" \
    --format 'platform={{.Os}}/{{.Architecture}} user={{.Config.User}} entrypoint={{json .Config.Entrypoint}} digests={{json .RepoDigests}}'
done

echo
echo "Next: pin the staging-tag digests in samples/ato-mail/images/images.lock and"
echo "capsule.toml, then preload the images on the dedicated Runner (see README.md)."
