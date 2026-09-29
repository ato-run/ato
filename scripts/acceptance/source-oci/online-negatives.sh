#!/usr/bin/env bash
# Egress-mode checks that exchange no application data with the Internet.
# Each case builds the D2 fixture (frozen python:3.12-alpine base) with an
# egress allowlist and one inserted RUN step, through `ato __source-oci-build`
# (root). Every case first runs the builder's preflight from the build
# network (direct dials blocked, unlisted CONNECT 403, listed CONNECT 200).
#
# usage: sudo online-negatives.sh ATO_BIN OUT_ROOT BASE_ARCHIVE RUN_USER
set -euo pipefail
ATO="$1"; OUT="$2"; BASE="$3"; RUN_USER="$4"
REPO="$(cd "$(dirname "$0")/../../.." && pwd)"
[[ $EUID -eq 0 ]] || { echo "run as root" >&2; exit 77; }
[[ -e "$OUT" ]] && { echo "refusing to reuse $OUT" >&2; exit 73; }
install -d -o "$RUN_USER" "$OUT"; install -d -m 0700 /srv/ato-src
ROOT='sha256:c4634f578a412db396771b61b064c6e546c9d6414c7fb5b1b05d5871f1885f7b'
BASE_SHA="sha256:$(sha256sum "$BASE" | cut -d' ' -f1)"
echo "host_before $(iptables -S | wc -l) rules, $(ip -o link | wc -l) links" > "$OUT/host.txt"

run_case() { # $1 name, $2 RUN line, $3 max_transfer_bytes
  local W="$OUT/$1"; install -d "$W/src/app"
  cp "$REPO/apps/portable-application/fixtures/source-oci/"* "$W/src/app/"
  python3 - "$W/src/app/Dockerfile" "$2" <<'PY'
import sys
p, extra = sys.argv[1], sys.argv[2]
s = open(p).read()
open(p, "w").write(s.replace("COPY index.html", extra + "\nCOPY index.html", 1))
PY
  ( cd "$W/src" && tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -czf "$W/source.tar.gz" app )
  cat > "$W/request.json" <<JSON
{"schema":"ato.source-oci-request/1","title":"egress $1","source_archive":"$W/source.tar.gz",
 "source_archive_sha256":"sha256:$(sha256sum "$W/source.tar.gz" | cut -d' ' -f1)","dockerfile":"Dockerfile","platform":"linux/amd64",
 "base_images":[{"reference":"python:3.12-alpine@$ROOT","pinned_digest":"$ROOT","archive":"$BASE","archive_sha256":"$BASE_SHA"}],
 "declared_transport_port":8080,
 "policy":{"network":"egress_allowlist","egress":{"hosts":["dl-cdn.alpinelinux.org","registry.npmjs.org"],"ports":[443],"max_transfer_bytes":$3},
   "build_timeout_seconds":300,"max_archive_bytes":134217728,
   "build":{"memory_bytes":2147483648,"cpu_limit_millis":2000,"pids_limit":1024,"disk_bytes":4294967296},
   "runtime":{"memory_bytes":268435456,"cpu_limit_millis":500,"pids_limit":128}}}
JSON
  set +e
  env -i PATH=/usr/bin:/bin "$ATO" __source-oci-build --request "$W/request.json" \
    --work-root "/srv/ato-src/$1-$$" --out "$W/out" > "$W/build.out" 2>&1
  local rc=$?
  set -e
  local egress="{}"
  [[ -f "$W/out/source-oci-provenance.json" ]] && egress=$(python3 -c "import json,sys;print(json.dumps(json.load(open(sys.argv[1]))['egress']))" "$W/out/source-oci-provenance.json")
  printf '{"case":"%s","exit":%d,"error":%s,"sessions_left":%d,"cgroups_left":%d,"egress":%s}\n' \
    "$1" "$rc" "$(grep -m1 '^error' "$W/build.out" | cut -c1-400 | python3 -c 'import json,sys;print(json.dumps(sys.stdin.read().strip()))')" \
    "$(ls /srv/ato-src | wc -l)" "$(find /sys/fs/cgroup -maxdepth 1 -name 'ato-source-oci-*' | wc -l)" "$egress" | tee "$W/result.json"
}
PROXY_PARTS='P=${HTTPS_PROXY#http://}; H=${P%:*}; PT=${P##*:}'
# RUN steps can reach nothing but the gate: without the proxy variables a
# listed host is unreachable, and a raw IP dial fails. The build succeeds
# only if every bypass attempt fails.
run_case bypass_blocked \
  'RUN if env -u HTTPS_PROXY -u https_proxy -u HTTP_PROXY -u http_proxy wget -q -T 5 -O /dev/null https://dl-cdn.alpinelinux.org/ ; then echo BYPASS_DNS; exit 1; fi; if nc -w 3 151.101.2.132 443 </dev/null; then echo BYPASS_IP; exit 1; fi; echo NO_BYPASS' \
  104857600
# An unlisted host through the gate: CONNECT refused, the build fails.
run_case unlisted_host 'RUN wget -q -T 10 -O /dev/null https://example.com/' 104857600
# The shared transfer budget: bytes relayed toward a listed host count, and
# crossing 1 KiB cuts the tunnel; the result is source_oci_egress_bound.
run_case transfer_budget \
  "RUN $PROXY_PARTS; (printf 'CONNECT dl-cdn.alpinelinux.org:443 HTTP/1.1\r\nHost: dl-cdn.alpinelinux.org:443\r\n\r\n'; sleep 1; head -c 4096 /dev/zero) | nc -w 5 \$H \$PT >/dev/null; true" \
  1024
echo "host_after $(iptables -S | wc -l) rules, $(ip -o link | wc -l) links" >> "$OUT/host.txt"
echo "tagged_rules_left $(iptables -S | grep -c ato-source-oci-egress || true)" >> "$OUT/host.txt"
echo "veths_left $(ip -o link | grep -c 'asoh' || true)" >> "$OUT/host.txt"
chown -R "$RUN_USER" "$OUT"; cat "$OUT/host.txt"
