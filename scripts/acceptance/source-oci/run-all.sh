#!/usr/bin/env bash
# 6b-D2 offline acceptance driver (run as root on a native Linux Docker host).
# usage: sudo run-all.sh ATO_BIN OUT_ROOT BASE_ARCHIVE RUN_USER
set -euo pipefail
ATO="$1"; OUT="$2"; BASE="$3"; RUN_USER="$4"
HERE="$(cd "$(dirname "$0")" && pwd)"; REPO="$(cd "$HERE/../../.." && pwd)"
[[ -e "$OUT" ]] && { echo "refusing to reuse $OUT" >&2; exit 73; }
install -d -o "$RUN_USER" "$OUT"
{
  echo "ato_sha256=$(sha256sum "$ATO" | cut -d' ' -f1)"
  echo "source_sha=$(git -C "$REPO" rev-parse HEAD 2>/dev/null || cat "$REPO/.source-sha")"
  echo "base_archive_sha256=$(sha256sum "$BASE" | cut -d' ' -f1)"
  echo "kernel=$(uname -srmo)"; echo "docker=$(docker version --format '{{.Server.Version}}' 2>/dev/null)"
} > "$OUT/environment.txt"
# 1. Positive build.
"$HERE/offline.sh" "$ATO" "$OUT/build" "$BASE" "$RUN_USER" build
runuser -u "$RUN_USER" -- "$ATO" pack "$OUT/build/out/authored" -o "$OUT/app.capsule" > "$OUT/pack.txt"
DERIVATION=$(sed -n 's/^derivation_ref=//p' "$OUT/pack.txt")
runuser -u "$RUN_USER" -- "$ATO" export "$OUT/app.capsule" --portability offline \
  --oci-archive "$OUT/build/out/image.tar" -o "$OUT/offline.capsule" > "$OUT/export.txt"
# 2. Two independent air-gapped runs from empty private stores.
for n in 1 2; do
  set +e
  "$REPO/scripts/portable-offline-oci-airgap.sh" --ato "$ATO" --bundle "$OUT/offline.capsule" \
    --derivation "$DERIVATION" --work-root "$OUT/run-$n" --run-user "$RUN_USER" > "$OUT/run-$n.log" 2>&1
  echo "run-$n exit=$?" | tee -a "$OUT/runs.txt"
  set -e
done
# 3. Negative cases, each on a new private daemon.
for c in unauthorized_external_input context_escape base_digest_mismatch build_timeout \
         archive_bound build_network privileged_run host_network_run production_socket store_not_empty; do
  "$HERE/offline.sh" "$ATO" "$OUT/neg-$c" "$BASE" "$RUN_USER" "$c" > "$OUT/neg-$c.txt" 2>&1 || true
done
# Host daemon untouched: record its image count before/after is the caller's job.
chown -R "$RUN_USER" "$OUT" || true
echo done
