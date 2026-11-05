#!/bin/sh
set -eu
umask 077

base=/data/freeeed-preview
mkdir -p "$base/cache" "$base/jobs"
expected=$(cat /capsule/engine.sha256)
engine="$base/cache/engine.jar"
if [ -e "$engine" ]; then
  actual=$(sha256sum "$engine" | cut -d ' ' -f 1)
  [ "$actual" = "$expected" ] || { echo 'engine cache mismatch' >&2; exit 1; }
else
  assembly=$(mktemp -d "$base/cache/.assembly.XXXXXXXX")
  trap 'rm -rf "$assembly"' EXIT
  cat /capsule/engine/part-* > "$assembly/engine.jar"
  actual=$(sha256sum "$assembly/engine.jar" | cut -d ' ' -f 1)
  [ "$actual" = "$expected" ] || { echo 'engine digest mismatch' >&2; exit 1; }
  mv "$assembly/engine.jar" "$engine"
  rmdir "$assembly"
  trap - EXIT
fi

java -Djava.io.tmpdir="$base/jobs" -Duser.home="$base/jobs" \
  -cp /capsule/preview TextOnlyExtractor 9998 &
extractor_pid=$!
trap 'kill "$extractor_pid" 2>/dev/null || true' EXIT HUP INT TERM
notices=false
if [ -s /capsule/source/third-party-notices.zip ]; then notices=true; fi
java -Djava.io.tmpdir="$base/jobs" -Duser.home="$base/jobs" \
  -Dfreeeed.preview.directText=true -Dfreeeed.preview.noticesIncluded="$notices" -cp /capsule/preview ProcessingPreview \
  "$engine" "$base/jobs" 0.0.0.0 8080 http://127.0.0.1:9998 /capsule/source.zip
