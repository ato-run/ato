#!/usr/bin/env bash
set -euo pipefail

CLI_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ATO_BIN="${ATO_BIN:-${CLI_DIR}/target/debug/ato}"

if [[ ! -f "${CLI_DIR}/Cargo.toml" ]]; then
  echo "[ERROR] Cargo.toml が見つかりません: ${CLI_DIR}" >&2
  exit 1
fi

if [[ ! -x "${ATO_BIN}" ]]; then
  echo "[INFO] ato バイナリが見つからないためビルドします"
  (cd "${CLI_DIR}" && cargo build -p ato-cli >/dev/null)
fi

SMOKE_ROOT="$(mktemp -d /tmp/ato-smoke-XXXXXX)"
SMOKE_HOME="${SMOKE_ROOT}/home"
SMOKE_WORK="${SMOKE_ROOT}/work"
REGISTRY_DATA="${SMOKE_ROOT}/registry"
mkdir -p "${SMOKE_HOME}" "${SMOKE_WORK}" "${REGISTRY_DATA}"

pick_port() {
  python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
}

REGISTRY_PORT="$(pick_port)"
REGISTRY_URL="http://127.0.0.1:${REGISTRY_PORT}"
REGISTRY_LOG="${SMOKE_ROOT}/registry.log"

PASS=0
FAIL=0

pass() {
  PASS=$((PASS + 1))
  echo "[PASS] $1"
}

fail() {
  FAIL=$((FAIL + 1))
  echo "[FAIL] $1" >&2
  if [[ -n "${2:-}" ]]; then
    echo "       $2" >&2
  fi
}

run_ok() {
  local name="$1"
  shift
  local out
  if out="$("$@" 2>&1)"; then
    pass "$name"
    printf '%s\n' "$out"
    return 0
  fi
  fail "$name" "${out}"
  printf '%s\n' "$out"
  return 1
}

run_fail_contains() {
  local name="$1"
  local expected="$2"
  shift 2
  local out
  if out="$("$@" 2>&1)"; then
    fail "$name" "失敗を期待しましたが成功しました"
    printf '%s\n' "$out"
    return 1
  fi
  if grep -Fq "$expected" <<<"$out"; then
    pass "$name"
  else
    fail "$name" "期待文字列が見つかりません: ${expected}"
  fi
  printf '%s\n' "$out"
  return 0
}

cleanup() {
  if [[ -n "${REGISTRY_PID:-}" ]] && kill -0 "${REGISTRY_PID}" 2>/dev/null; then
    kill "${REGISTRY_PID}" >/dev/null 2>&1 || true
    wait "${REGISTRY_PID}" 2>/dev/null || true
  fi
  rm -rf "${SMOKE_ROOT}"
}
trap cleanup EXIT

echo "[INFO] smoke root: ${SMOKE_ROOT}"
echo "[INFO] registry: ${REGISTRY_URL}"

HOME="${SMOKE_HOME}" "${ATO_BIN}" registry serve --host 127.0.0.1 --port "${REGISTRY_PORT}" --data-dir "${REGISTRY_DATA}" >"${REGISTRY_LOG}" 2>&1 &
REGISTRY_PID=$!

for _ in {1..50}; do
  if lsof -nP -iTCP:"${REGISTRY_PORT}" -sTCP:LISTEN >/dev/null 2>&1; then
    break
  fi
  sleep 0.2
done

if lsof -nP -iTCP:"${REGISTRY_PORT}" -sTCP:LISTEN >/dev/null 2>&1; then
  pass "Phase1: registry serve 起動"
else
  fail "Phase1: registry serve 起動" "${REGISTRY_LOG} を確認してください"
fi

run_ok "Phase1: config set registry.url" \
  env HOME="${SMOKE_HOME}" "${ATO_BIN}" config set registry.url "${REGISTRY_URL}" >/dev/null

CONFIG_PATH="${SMOKE_HOME}/.ato/config.toml"
if [[ -f "${CONFIG_PATH}" ]] && grep -Fq "url = \"${REGISTRY_URL}\"" "${CONFIG_PATH}"; then
  pass "Phase1: config.toml に registry.url 保存"
else
  fail "Phase1: config.toml に registry.url 保存"
fi

if [[ -n "${ATO_SMOKE_LOGIN_TOKEN:-}" ]]; then
  run_ok "Phase1: login" env HOME="${SMOKE_HOME}" "${ATO_BIN}" login --token "${ATO_SMOKE_LOGIN_TOKEN}" >/dev/null
  run_ok "Phase1: whoami" env HOME="${SMOKE_HOME}" "${ATO_BIN}" whoami >/dev/null
else
  echo "[INFO] ATO_SMOKE_LOGIN_TOKEN 未設定のため login/whoami はスキップ"
fi

run_ok "Phase2: init" env HOME="${SMOKE_HOME}" bash -lc "cd '${SMOKE_WORK}' && '${ATO_BIN}' init smoke-test-pkg" >/dev/null

SMOKE_PKG_DIR="${SMOKE_WORK}/smoke-test-pkg"

run_ok "Phase2: build" env HOME="${SMOKE_HOME}" bash -lc "cd '${SMOKE_PKG_DIR}' && '${ATO_BIN}' build" >/dev/null
run_ok "Phase2: key gen" env HOME="${SMOKE_HOME}" bash -lc "cd '${SMOKE_PKG_DIR}' && '${ATO_BIN}' key gen" >/dev/null
run_ok "Phase2: publish(local)" env HOME="${SMOKE_HOME}" bash -lc "cd '${SMOKE_PKG_DIR}' && '${ATO_BIN}' publish --registry '${REGISTRY_URL}' --artifact ./smoke-test-pkg.capsule --scoped-id demo/smoke-test-pkg" >/dev/null

SEARCH_JSON="$(env TERM=dumb HOME="${SMOKE_HOME}" "${ATO_BIN}" --json search smoke-test-pkg --registry "${REGISTRY_URL}" 2>&1)"
if python3 -c 'import json,sys; obj=json.loads(sys.stdin.read()); assert isinstance(obj, list)' <<<"${SEARCH_JSON}" >/dev/null 2>&1
then
  pass "Phase3: search --json がJSON配列を返す"
else
  fail "Phase3: search --json がJSON配列を返す" "${SEARCH_JSON}"
fi

INSTALL_JSON="$(env TERM=dumb HOME="${SMOKE_HOME}" "${ATO_BIN}" install demo/smoke-test-pkg --registry "${REGISTRY_URL}" --json 2>&1)"
if python3 -c 'import json,sys; obj=json.loads(sys.stdin.read()); assert isinstance(obj, dict); assert obj.get("scoped_id") == "demo/smoke-test-pkg"' <<<"${INSTALL_JSON}" >/dev/null 2>&1
then
  pass "Phase3: install --json が期待形式"
else
  fail "Phase3: install --json が期待形式" "${INSTALL_JSON}"
fi

run_fail_contains "Phase3: run fail-closed (sandbox未指定)" "ATO_ERR_POLICY_VIOLATION" \
  env TERM=dumb HOME="${SMOKE_HOME}" "${ATO_BIN}" run demo/smoke-test-pkg --registry "${REGISTRY_URL}" --background --yes >/dev/null

ENGINE_ERR_OUTPUT="$(env TERM=dumb HOME="${SMOKE_HOME}" "${ATO_BIN}" run demo/smoke-test-pkg --registry "${REGISTRY_URL}" --background --yes --sandbox 2>&1 || true)"
if grep -Fq "ATO_ERR_ENGINE_MISSING" <<<"${ENGINE_ERR_OUTPUT}"; then
  pass "Phase3: run fail-closed (engine missing)"
else
  fail "Phase3: run fail-closed (engine missing)" "${ENGINE_ERR_OUTPUT}"
fi

if grep -Fq "ato config engine install --engine nacelle" <<<"${ENGINE_ERR_OUTPUT}"; then
  pass "Phase3: engine missing ヒント表示"
else
  fail "Phase3: engine missing ヒント表示" "${ENGINE_ERR_OUTPUT}"
fi

run_ok "Phase5: gen-ci" env TERM=dumb HOME="${SMOKE_HOME}" bash -lc "cd '${SMOKE_PKG_DIR}' && '${ATO_BIN}' gen-ci" >/dev/null
if [[ -f "${SMOKE_PKG_DIR}/.github/workflows/ato-publish.yml" ]]; then
  pass "Phase5: gen-ci が workflow ファイルを書き込む"
else
  fail "Phase5: gen-ci が workflow ファイルを書き込む"
fi

echo
echo "[SUMMARY] PASS=${PASS} FAIL=${FAIL}"
if [[ ${FAIL} -gt 0 ]]; then
  exit 1
fi
