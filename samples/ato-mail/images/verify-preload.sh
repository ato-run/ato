#!/usr/bin/env bash
# Verify that the images a Runner preloaded really are the D1 pins.
#
#   PLATFORM=linux/amd64 ./verify-preload.sh <stalwart-ref> <missive-ref>
#
# Checks, for each image: digest/platform (as reported by docker), the expected
# user, a non-empty ENTRYPOINT, and that the Stalwart binary carries no file
# capability (the Runner execs it under cap-drop=ALL). Exits non-zero on the
# first mismatch; a mismatch is an admission/preload problem, not a Contract
# failure.
set -euo pipefail

stalwart_ref=${1:?usage: verify-preload.sh <stalwart-ref> <missive-ref>}
missive_ref=${2:?usage: verify-preload.sh <stalwart-ref> <missive-ref>}
platform=${PLATFORM:-linux/amd64}
failures=0

check() {
  local what=$1 expected=$2 actual=$3
  if [ "${expected}" = "${actual}" ]; then
    echo "ok   ${what}: ${actual}"
  else
    echo "FAIL ${what}: expected ${expected}, got ${actual}" >&2
    failures=$((failures + 1))
  fi
}

verify_image() {
  local name=$1 ref=$2 user=$3 inspect image_platform image_user entrypoint
  echo "== ${name}: ${ref}"
  if ! inspect=$(docker image inspect "${ref}" --format \
    '{{.Os}}/{{.Architecture}}|{{.Config.User}}|{{json .Config.Entrypoint}}' 2>/dev/null); then
    echo "FAIL ${name}: image not present" >&2
    failures=$((failures + 1))
    return
  fi
  image_platform=${inspect%%|*}
  inspect=${inspect#*|}
  image_user=${inspect%%|*}
  entrypoint=${inspect#*|}
  check "${name} platform" "${platform}" "${image_platform}"
  check "${name} user" "${user}" "${image_user}"
  if [ "${entrypoint}" = "null" ] || [ -z "${entrypoint}" ]; then
    echo "FAIL ${name} entrypoint: missing (argv would become the command)" >&2
    failures=$((failures + 1))
  else
    echo "ok   ${name} entrypoint: ${entrypoint}"
  fi
}

verify_image stalwart "${stalwart_ref}" "1000:1000"
verify_image missive "${missive_ref}" "65532:65532"

# The real condition is exec under cap-drop=ALL, not the xattr itself: the
# Runner combines cap-drop=ALL with no-new-privileges. If the binary still has
# a file capability the kernel refuses the exec, so --version is the check.
if docker run --rm --cap-drop=ALL --entrypoint /usr/local/bin/stalwart \
  "${stalwart_ref}" --version >/dev/null 2>&1; then
  echo "ok   stalwart binary: execs under cap-drop=ALL"
else
  echo "FAIL stalwart binary: exec under cap-drop=ALL failed (file capability still set?)" >&2
  failures=$((failures + 1))
fi

if [ "${failures}" -ne 0 ]; then
  echo "${failures} preload mismatch(es)" >&2
  exit 1
fi
echo "preload verified"
