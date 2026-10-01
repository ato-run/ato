#!/usr/bin/env bash
# 6b-D2 offline acceptance driver (run as root on a native Linux Docker host).
# usage: sudo run-all.sh ATO_BIN OUT_ROOT BASE_ARCHIVE RUN_USER
set -euo pipefail
ATO="$1"; OUT="$2"; BASE="$3"; RUN_USER="$4"
HERE="$(cd "$(dirname "$0")" && pwd)"; REPO="$(cd "$HERE/../../.." && pwd)"
[[ -e "$OUT" ]] && { echo "refusing to reuse $OUT" >&2; exit 73; }
install -d -o "$RUN_USER" "$OUT"
host_state() { echo "images=$(docker image ls -aq | wc -l) containers=$(docker ps -aq | wc -l) volumes=$(docker volume ls -q | wc -l)"; }
{
  echo "ato_sha256=$(sha256sum "$ATO" | cut -d' ' -f1)"
  echo "source_sha=$(git -C "$REPO" rev-parse HEAD 2>/dev/null || cat "$REPO/.source-sha")"
  echo "base_archive_sha256=$(sha256sum "$BASE" | cut -d' ' -f1)"
  echo "kernel=$(uname -srmo)"; echo "docker=$(docker version --format '{{.Server.Version}}' 2>/dev/null)"
  echo "host_before: $(host_state)"
} > "$OUT/environment.txt"
# 1. Positive build (the CLI starts and releases its own private session).
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
# 3. Negative cases, each with its own session.
for c in unauthorized_external_input context_escape base_digest_mismatch base_graph_mismatch \
         build_timeout client_disconnect archive_bound disk_bound log_bound build_network \
         privileged_run host_network_run remote_input_reachable remote_registry_reachable \
         socket_alias non_root; do
  "$HERE/offline.sh" "$ATO" "$OUT/neg-$c" "$BASE" "$RUN_USER" "$c" > "$OUT/neg-$c.txt" 2>&1 || true
done
echo "host_after: $(host_state)" >> "$OUT/environment.txt"
chown -R "$RUN_USER" "$OUT" || true
echo done
