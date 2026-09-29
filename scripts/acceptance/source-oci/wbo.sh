#!/usr/bin/env bash
# WBO: the approved isolated online build and runs of its saved artifact.
# Expectations are fixed in docs/ops/formation-wbo-expectations.json.
#
# usage: sudo wbo.sh ATO_BIN OUT_ROOT RUN_USER WBO_ARCHIVE
set -euo pipefail
ATO="$1"; OUT="$2"; RUN_USER="$3"; WBO="$4"
HERE="$(cd "$(dirname "$0")" && pwd)"; REPO="$(cd "$HERE/../../.." && pwd)"
[[ $EUID -eq 0 ]] || { echo "run as root" >&2; exit 77; }
[[ -e "$OUT" ]] && { echo "refusing to reuse $OUT" >&2; exit 73; }
install -d -o "$RUN_USER" "$OUT"; install -d -m 0700 /srv/ato-src
as_user() { runuser -u "$RUN_USER" -- env -i PATH=/usr/bin:/bin HOME="$OUT/home" "$@"; }
install -d -o "$RUN_USER" "$OUT/home"
host_state() { echo "images=$(docker image ls -aq | wc -l) containers=$(docker ps -aq | wc -l) volumes=$(docker volume ls -q | wc -l) iptables=$(iptables -S | wc -l) links=$(ip -o link | wc -l)"; }
# 0. Preconditions: exact source bytes and room for the job plus teardown.
AVAIL=$(df --output=avail -B1 / | tail -1)
{
  echo "ato_sha256=$(sha256sum "$ATO" | cut -d' ' -f1)"
  echo "source_sha=$(git -C "$REPO" rev-parse HEAD 2>/dev/null || true)"
  echo "wbo_archive_sha256=sha256:$(sha256sum "$WBO" | cut -d' ' -f1)"
  echo "disk_available_bytes=$AVAIL"
  echo "host_before: $(host_state)"
} > "$OUT/environment.txt"
[[ "$(sed -n 's/^wbo_archive_sha256=//p' "$OUT/environment.txt")" == "sha256:8af51caaa6b8434a06e7a41753ae54381ba1595c362050e8c1d1836dfd604d65" ]] \
  || { echo "WBO archive is not the pinned bytes" >&2; exit 65; }
# 5 GiB job disk + 128 MiB archive + bundles + base, and 4 GiB for teardown.
(( AVAIL > 12 * 1024 * 1024 * 1024 )) || { echo "not enough disk headroom ($AVAIL)" >&2; exit 75; }
cp "$WBO" "$OUT/wbo-source.tar.gz"; chown "$RUN_USER" "$OUT/wbo-source.tar.gz"

# 1. Base acquisition: its own phase and allowlist, once.
set +e
as_user "$ATO" __source-oci-acquire-base --reference docker.io/library/node:24-alpine \
  --platform linux/amd64 --max-bytes 209715200 \
  --out "$OUT/base-node.tar" --provenance "$OUT/base-node.json" > "$OUT/1-acquire.txt" 2>&1
RC=$?
set -e
echo "acquire exit=$RC" | tee "$OUT/summary.txt"
[[ $RC -eq 0 ]] || exit 0
ROOT=$(sed -n 's/^root_digest=//p' "$OUT/1-acquire.txt")
BASE_SHA=$(sed -n 's/^archive_sha256=//p' "$OUT/1-acquire.txt")
MOVED=$(sed -n 's/^transferred_bytes=//p' "$OUT/1-acquire.txt")
REMAINING=$((524288000 - MOVED))
echo "base root=$ROOT moved=$MOVED build_egress_budget=$REMAINING" | tee -a "$OUT/summary.txt"

# 2. The build (A).
cat > "$OUT/request.json" <<JSON
{"schema":"ato.source-oci-request/1","title":"WBO","source_archive":"$OUT/wbo-source.tar.gz",
 "source_archive_sha256":"sha256:8af51caaa6b8434a06e7a41753ae54381ba1595c362050e8c1d1836dfd604d65",
 "dockerfile":"Dockerfile","platform":"linux/amd64",
 "base_images":[{"reference":"node:24-alpine","pinned_digest":"$ROOT","archive":"$OUT/base-node.tar","archive_sha256":"$BASE_SHA"}],
 "declared_transport_port":80,
 "authorized_state":[{"id":"server_data","mount":"/opt/app/server-data"}],
 "policy":{"network":"egress_allowlist",
   "egress":{"hosts":["dl-cdn.alpinelinux.org","registry.npmjs.org"],"ports":[443],"max_transfer_bytes":$REMAINING},
   "build_timeout_seconds":900,"max_archive_bytes":134217728,
   "build":{"memory_bytes":4294967296,"cpu_limit_millis":4000,"pids_limit":2048,"disk_bytes":5368709120},
   "runtime":{"memory_bytes":536870912,"cpu_limit_millis":1000,"pids_limit":256}}}
JSON
set +e
env -i PATH=/usr/bin:/bin "$ATO" __source-oci-build --request "$OUT/request.json" \
  --work-root "/srv/ato-src/wbo-$$" --out "$OUT/build" > "$OUT/2-build.txt" 2>&1
RC=$?
set -e
echo "A build exit=$RC $(grep -m1 '^error' "$OUT/2-build.txt" | cut -c1-300)" | tee -a "$OUT/summary.txt"
echo "after_build: sessions=$(ls /srv/ato-src | wc -l) cgroups=$(find /sys/fs/cgroup -maxdepth 1 -name 'ato-source-oci-*' | wc -l) tagged_rules=$(iptables -S | grep -c ato-source-oci-egress || true) veths=$(ip -o link | grep -c asoh || true)" | tee -a "$OUT/summary.txt"
chown -R "$RUN_USER" "$OUT"
[[ $RC -eq 0 ]] || exit 0

# 3. Pack and export the saved artifact offline.
as_user "$ATO" pack "$OUT/build/authored" -o "$OUT/app.capsule" > "$OUT/3-pack.txt"
D=$(sed -n 's/^derivation_ref=//p' "$OUT/3-pack.txt")
as_user "$ATO" export "$OUT/app.capsule" --portability offline \
  --oci-archive "$OUT/build/image.tar" -o "$OUT/offline.capsule" > "$OUT/3-export.txt"

# 4. B-E on a new private store with no route out.
set +e
unshare --net "$HERE/wbo-run.sh" "$ATO" "$OUT" "$RUN_USER" "$D" > "$OUT/4-run.log" 2>&1
echo "runtime exit=$?" | tee -a "$OUT/summary.txt"
set -e
grep -E '^(B|C|D|E|identity|state|store)' "$OUT/4-run.log" | tee -a "$OUT/summary.txt"
echo "host_after: $(host_state)" >> "$OUT/environment.txt"
chown -R "$RUN_USER" "$OUT"
