#!/usr/bin/env bash
# Build one HeyPuter/everything app (MIT) for ato.run as one HTML file. Puter.js is replaced by ../puter-compat/ato-puter-shim.js, so data
# is kept in the ato.run Instance (window.storage) instead of a Puter account.
#
# Usage: samples/discover-oss/puter-compat/build-app.sh <app> <new-output.html>
#   <app>: property-management | crm | contracts | invoicing
set -euo pipefail

UPSTREAM=https://github.com/HeyPuter/everything.git
COMMIT=c17d23be5ab4141742139c227ab2fd592397985e   # main on 2026-11-06
SINGLEFILE=vite-plugin-singlefile@2.3.3   # supports vite 5-8

here=$(cd "$(dirname "$0")" && pwd)
shim="$here/ato-puter-shim.js"
case "${1:-}" in property-management|crm|contracts|invoicing) APP="src/$1" ;;
  *) echo "usage: build-app.sh <property-management|crm|contracts|invoicing> <out.html>" >&2; exit 2 ;; esac
out=${2:?usage: build-app.sh <app> <new-output.html>}
case "$out" in /*) ;; *) out="$PWD/$out" ;; esac
[ -e "$out" ] && { echo "refusing to overwrite $out" >&2; exit 1; }

work=$(mktemp -d "${TMPDIR:-$PWD/.tmp}/puter-app.XXXXXX")
git -C "$work" init -q
git -C "$work" remote add origin "$UPSTREAM"
git -C "$work" fetch -q --depth 1 --filter=blob:none origin "$COMMIT"
git -C "$work" sparse-checkout set "$APP" dev   # vite.config.js imports ../../dev
git -C "$work" checkout -q FETCH_HEAD
[ "$(git -C "$work" rev-parse HEAD)" = "$COMMIT" ]

cd "$work/$APP"
SHIM="$shim" python3 -I - <<'EOF'
import os, re
html = open('index.html').read()
shim = open(os.environ['SHIM']).read()
# Upstream apps load Puter.js with or without `defer`; the shim must run first.
tag = re.compile(r'<script\s+src="https://js\.puter\.com/v2/?"[^>]*>\s*</script>')
html, n = tag.subn('<!--ato-puter-shim-->', html, count=1)
assert n == 1, 'Puter.js script tag not found'
assert not re.search(r'<script[^>]+js\.puter\.com', html), 'Puter.js is still loaded'
html = html.replace('<!--ato-puter-shim-->', '<script>\n' + shim + '\n</script>', 1)
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
echo "built $APP@$COMMIT -> $out"
