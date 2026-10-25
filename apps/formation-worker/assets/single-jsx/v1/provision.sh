#!/bin/sh
# Put single-jsx/v1 on a builder.
#
# Run at DEPLOY time, with a network. The build itself never has one: that is
# the whole point of a platform-managed compiler, and a build that fetched its
# own toolchain would make "no network" a claim rather than a property.
#
# Idempotent, and verified: every file is checked against VENDOR.lock before it
# is installed, so a CDN that served something else does not silently become
# what this preset means.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
destination=${1:-/opt/ato/assets/single-jsx/v1}

mkdir -p "$destination"
cp "$here/compile.mjs" "$here/index.html.tmpl" "$here/VENDOR.lock" "$destination/"

# `sha256sum` on Linux, `shasum -a 256` on macOS. Both print the same shape.
if command -v sha256sum >/dev/null 2>&1; then
  digest() { sha256sum "$1" | cut -d' ' -f1; }
else
  digest() { shasum -a 256 "$1" | cut -d' ' -f1; }
fi

grep -v '^#' "$here/VENDOR.lock" | grep -v '^[[:space:]]*$' | while read -r want name url; do
  target="$destination/$name"
  if [ -f "$target" ] && [ "$(digest "$target")" = "$want" ]; then
    echo "ok       $name"
    continue
  fi
  tmp="$destination/.$name.partial"
  curl -fsSL --proto '=https' --tlsv1.2 -o "$tmp" "$url"
  got=$(digest "$tmp")
  if [ "$got" != "$want" ]; then
    rm -f "$tmp"
    echo "REFUSED  $name: expected $want, got $got" >&2
    exit 1
  fi
  mv "$tmp" "$target"
  echo "fetched  $name"
done

# Read-only for everyone: the build sandbox binds this tree read-only, and the
# permissions should say the same thing to anyone who looks at the host.
chmod -R a-w "$destination"
echo "single-jsx/v1 provisioned at $destination"
