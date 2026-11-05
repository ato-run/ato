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
  trap 'exit 1' HUP INT TERM
  cat /capsule/engine/part-* > "$assembly/engine.jar"
  actual=$(sha256sum "$assembly/engine.jar" | cut -d ' ' -f 1)
  [ "$actual" = "$expected" ] || { echo 'engine digest mismatch' >&2; exit 1; }
  mv "$assembly/engine.jar" "$engine"
  rmdir "$assembly"
  trap - EXIT HUP INT TERM
fi
exec java -Djava.io.tmpdir="$base/jobs" -Duser.home="$base/jobs" \
  -cp /capsule/preview ProcessingPreview "$engine" "$base/jobs" 0.0.0.0 8080 http://tika:9998
