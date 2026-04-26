#!/usr/bin/env bash
set -euo pipefail

# eBPF integration test:
# 1) verify cgroup egress program attach
# 2) verify localhost allow entry exists in IPV4_ALLOW map
# 3) verify localhost HTTP reachability

ATO_BIN="${ATO_BIN:-/tmp/ato}"
APP_DIR="${APP_DIR:-/tmp/ato-linux-web-test-HUsr5W/web-python}"
SCOPED_ID="${SCOPED_ID:-}"
REGISTRY_URL="${REGISTRY_URL:-https://staging.api.ato.run}"
PORT="${PORT:-38282}"
BPFT="${BPFT:-/usr/local/sbin/bpftool}"
TIMEOUT_SEC="${TIMEOUT_SEC:-20}"
LOG_DIR="${LOG_DIR:-/tmp/nacelle-ebpf-test}"

mkdir -p "${LOG_DIR}"
RUN_LOG="${LOG_DIR}/ato_run.log"

info() { printf '[INFO] %s\n' "$*"; }
pass() { printf '[PASS] %s\n' "$*"; }
fail() { printf '[FAIL] %s\n' "$*" >&2; exit 1; }

cleanup() {
  sudo pkill -f "${ATO_BIN} run --sandbox" >/dev/null 2>&1 || true
  sudo pkill -f "uv run --offline python3 main.py" >/dev/null 2>&1 || true
  sudo pkill -f "python3 main.py" >/dev/null 2>&1 || true
  for pid in $(sudo ss -ltnp 2>/dev/null | grep "127.0.0.1:${PORT}" | sed -n 's/.*pid=\([0-9]\+\).*/\1/p' | sort -u); do
    sudo kill -9 "${pid}" >/dev/null 2>&1 || true
  done
}
trap cleanup EXIT

command -v "${ATO_BIN}" >/dev/null 2>&1 || fail "ato binary not found: ${ATO_BIN}"
if ! command -v "${BPFT}" >/dev/null 2>&1; then
  BPFT="$(command -v bpftool || true)"
fi
[ -n "${BPFT}" ] || fail "bpftool not found"
if ! sudo "${BPFT}" version >/dev/null 2>&1; then
  fail "bpftool is not runnable (${BPFT}). install a real bpftool binary (not wrapper-only)."
fi
command -v jq >/dev/null 2>&1 || fail "jq not found"
command -v curl >/dev/null 2>&1 || fail "curl not found"
command -v ss >/dev/null 2>&1 || fail "ss not found"

if [ -z "${SCOPED_ID}" ]; then
  [ -d "${APP_DIR}" ] || fail "app dir not found: ${APP_DIR}"
fi

info "Starting Tier2 sandbox app via ato..."
cleanup
if [ -n "${SCOPED_ID}" ]; then
  sudo env PATH="$HOME/.deno/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH" \
    "${ATO_BIN}" run --sandbox "${SCOPED_ID}" --registry "${REGISTRY_URL}" -y >"${RUN_LOG}" 2>&1 &
else
  (
    cd "${APP_DIR}"
    sudo env PATH="$HOME/.deno/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH" \
      "${ATO_BIN}" run --sandbox . -y >"${RUN_LOG}" 2>&1 &
  )
fi

for _ in $(seq 1 "${TIMEOUT_SEC}"); do
  if ss -ltn | grep -q "127.0.0.1:${PORT}"; then
    break
  fi
  sleep 1
done
ss -ltn | grep -q "127.0.0.1:${PORT}" || {
  tail -n 120 "${RUN_LOG}" || true
  fail "listener not ready on 127.0.0.1:${PORT}"
}
pass "listener is up on 127.0.0.1:${PORT}"

TARGET_PID="$(
  sudo ss -ltnp | grep "127.0.0.1:${PORT}" | sed -n 's/.*pid=\([0-9]\+\).*/\1/p' | head -n 1
)"
[ -n "${TARGET_PID}" ] || {
  ps -eo pid,ppid,stat,comm,args | egrep "ato run --sandbox|capsule-dev-|uv run --offline python3 main.py|python3 main.py|deno|node" || true
  fail "listener pid not found"
}
info "listener pid: ${TARGET_PID}"

CGROUP_REL="$(sudo awk -F: '$1=="0"{print $3}' "/proc/${TARGET_PID}/cgroup")"
[ -n "${CGROUP_REL}" ] || fail "failed to resolve cgroup path"
CGROUP_PATH="/sys/fs/cgroup${CGROUP_REL}"
info "cgroup path: ${CGROUP_PATH}"

info "Checking cgroup eBPF attach..."
info "bpftool cgroup tree (for visualization):"
sudo "${BPFT}" cgroup tree
sudo "${BPFT}" cgroup show "${CGROUP_PATH}" -j >"${LOG_DIR}/cgroup_show.json"
cat "${LOG_DIR}/cgroup_show.json"

EGRESS_PROG_ID="$(
  jq -r '.[] | select(.attach_type == "egress" or .attach_type == "cgroup_inet_egress") | .id' "${LOG_DIR}/cgroup_show.json" | head -n 1
)"
[ -n "${EGRESS_PROG_ID}" ] || fail "no egress program attached to ${CGROUP_PATH}"
pass "egress program attached (prog id: ${EGRESS_PROG_ID})"

info "Resolving IPV4_ALLOW map id..."
sudo "${BPFT}" map show -j >"${LOG_DIR}/map_show.json"
MAP_ID="$(
  jq -r '.[] | select(.name == "IPV4_ALLOW") | .id' "${LOG_DIR}/map_show.json" | head -n 1
)"
[ -n "${MAP_ID}" ] || fail "IPV4_ALLOW map not found"
pass "IPV4_ALLOW map found (id: ${MAP_ID})"

info "Checking localhost (127.0.0.0/8) entry in IPV4_ALLOW..."
sudo "${BPFT}" map dump id "${MAP_ID}" -j >"${LOG_DIR}/map_dump_ipv4_allow.json"
cat "${LOG_DIR}/map_dump_ipv4_allow.json"

jq -e '
  .[] |
  select((.key | length) == 8) |
  select(.key[0] == "0x08") |
  select(.key[4:8] == ["0x00","0x00","0x00","0x7f"])
' "${LOG_DIR}/map_dump_ipv4_allow.json" >/dev/null || {
  fail "127.0.0.0/8 not found in IPV4_ALLOW"
}
pass "127.0.0.0/8 exists in IPV4_ALLOW"

info "Testing localhost HTTP reachability..."
curl -sv --max-time 5 "http://127.0.0.1:${PORT}/" \
  -o "${LOG_DIR}/curl.out" 2>"${LOG_DIR}/curl.err" || {
  cat "${LOG_DIR}/curl.err" || true
  fail "curl failed"
}
cat "${LOG_DIR}/curl.err"
head -n 20 "${LOG_DIR}/curl.out" || true
pass "localhost HTTP request succeeded"

info "All eBPF localhost integration checks passed."
