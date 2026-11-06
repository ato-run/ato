#!/usr/bin/env bash
# Build HeyPuter/everything src/property-management (MIT) for ato.run as one
# HTML file. Puter.js is replaced by ../puter-compat/ato-puter-shim.js, so data
# is kept in the ato.run Instance (window.storage) instead of a Puter account.
#
# Usage: samples/discover-oss/puter-property-management/build.sh <new-output.html>
set -euo pipefail

UPSTREAM=https://github.com/HeyPuter/everything.git
COMMIT=c17d23be5ab4141742139c227ab2fd592397985e   # main on 2026-11-06
APP=src/property-management
SINGLEFILE=vite-plugin-singlefile@2.3.0

here=$(cd "$(dirname "$0")" && pwd)
shim="$here/../puter-compat/ato-puter-shim.js"
out=${1:?usage: build.sh <new-output.html>}
case "$out" in /*) ;; *) out="$PWD/$out" ;; esac
[ -e "$out" ] && { echo "refusing to overwrite $out" >&2; exit 1; }

work=$(mktemp -d "${TMPDIR:-$PWD/.tmp}/puter-pm.XXXXXX")
git -C "$work" init -q
git -C "$work" remote add origin "$UPSTREAM"
git -C "$work" fetch -q --depth 1 --filter=blob:none origin "$COMMIT"
git -C "$work" sparse-checkout set "$APP" dev   # vite.config.js imports ../../dev
git -C "$work" checkout -q FETCH_HEAD
[ "$(git -C "$work" rev-parse HEAD)" = "$COMMIT" ]

cd "$work/$APP"
TAG='<script src="https://js.puter.com/v2/"></script>'
grep -qF "$TAG" index.html
SHIM="$shim" TAG="$TAG" python3 -I - <<'EOF'
import os
html = open('index.html').read()
shim = open(os.environ['SHIM']).read()
html = html.replace(os.environ['TAG'], '<script>\n' + shim + '\n</script>', 1)
assert os.environ['TAG'] not in html
open('index.html', 'w').write(html)
EOF

cat > ato.vite.config.mjs <<'EOF'
import base from './vite.config.js';
import { viteSingleFile } from 'vite-plugin-singlefile';
const cfg = typeof base === 'function' ? base({ command: 'build', mode: 'production' }) : base;
export default { ...cfg, plugins: [...(cfg.plugins || []), viteSingleFile()] };
EOF

npm ci --ignore-scripts --no-audit --no-fund
npm install --no-save --ignore-scripts --no-audit --no-fund "$SINGLEFILE"
npx vite build --config ato.vite.config.mjs
cp dist/index.html "$out"
echo "built $COMMIT -> $out"
