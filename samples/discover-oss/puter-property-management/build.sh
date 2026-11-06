#!/usr/bin/env bash
# Build HeyPuter/everything src/property-management for ato.run, replacing
# Puter.js with samples/discover-oss/puter-compat/ato-puter-shim.js so data is
# kept in the ato.run Instance (window.storage) instead of a Puter account.
#
# Usage: samples/discover-oss/puter-property-management/build.sh <new-output-dir>
set -euo pipefail

UPSTREAM=https://github.com/HeyPuter/everything.git
COMMIT=c17d23be5ab4141742139c227ab2fd592397985e   # main on 2026-11-06
APP=src/property-management

here=$(cd "$(dirname "$0")" && pwd)
shim="$here/../puter-compat/ato-puter-shim.js"
out=${1:?usage: build.sh <new-output-dir>}
case "$out" in /*) ;; *) out="$PWD/$out" ;; esac
[ -e "$out" ] && { echo "refusing to overwrite $out" >&2; exit 1; }

work=$(mktemp -d "${TMPDIR:-$PWD/.tmp}/puter-pm.XXXXXX")
git -C "$work" init -q
git -C "$work" remote add origin "$UPSTREAM"
git -C "$work" fetch -q --depth 1 --filter=blob:none origin "$COMMIT"
git -C "$work" sparse-checkout set "$APP" dev
git -C "$work" checkout -q FETCH_HEAD
[ "$(git -C "$work" rev-parse HEAD)" = "$COMMIT" ]

cd "$work/$APP"
grep -q '<script src="https://js.puter.com/v2/"></script>' index.html
cp "$shim" public/ato-puter-shim.js
sed -i.bak 's#<script src="https://js.puter.com/v2/"></script>#<script src="./ato-puter-shim.js"></script>#' index.html
rm index.html.bak
! grep -q 'js.puter.com' index.html

npm ci --ignore-scripts --no-audit --no-fund
npm run build
cp "$work/LICENSE" dist/LICENSE 2>/dev/null || true
mkdir -p "$out"
cp -R dist/. "$out/"
echo "built $COMMIT -> $out"
