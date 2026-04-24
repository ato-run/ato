#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ATO_CLI_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
ATO_CLI="${ATO_CLI_DIR}/target/debug/ato"

GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[0;33m'
NC='\033[0m'

log_info() { echo -e "${GREEN}✓${NC} $1"; }
log_error() { echo -e "${RED}✗${NC} $1"; }
log_warn() { echo -e "${YELLOW}⚠${NC} $1"; }

mkdir -p "${ATO_CLI_DIR}/.ato/test-scratch"
E2E_WORK_DIR="$(mktemp -d "${ATO_CLI_DIR}/.ato/test-scratch/ato-zero-config-e2e-XXXX")"
cleanup() {
  rm -rf "${E2E_WORK_DIR}"
}
trap cleanup EXIT

ensure_cli() {
  if [ ! -x "${ATO_CLI}" ]; then
    log_info "Building ato-cli..."
    (cd "${ATO_CLI_DIR}" && cargo build -p ato-cli >/dev/null)
  fi
}

assert_contains() {
  local file="$1"
  local pattern="$2"
  local message="$3"
  if grep -q "${pattern}" "${file}"; then
    log_info "${message}"
  else
    log_error "${message}"
    echo "---- output (${file}) ----"
    cat "${file}"
    exit 1
  fi
}

assert_capsule_generated() {
  local dir="$1"
  local label="$2"
  local capsule
  capsule="$(find "${dir}" -maxdepth 1 -name "*.capsule" | head -n 1 || true)"
  if [ -z "${capsule}" ]; then
    log_error "${label}: .capsule artifact not generated"
    exit 1
  fi
  log_info "${label}: artifact generated (${capsule})"
}

run_zero_config_build_case() {
  local lang="$1"
  local dir="$2"
  local log_file="$3"

  if "${ATO_CLI}" build "${dir}" >"${log_file}" 2>&1; then
    log_info "${lang}: ato build exited 0"
  else
    log_error "${lang}: ato build failed"
    cat "${log_file}"
    exit 1
  fi

  assert_contains "${log_file}" "No capsule.toml found. Using defaults" "${lang}: warning emitted"
  assert_contains "${log_file}" "Smoke passed" "${lang}: smoke test passed"
  assert_capsule_generated "${dir}" "${lang}"
}

resolve_session_token() {
  if [ -n "${ATO_TOKEN:-}" ]; then
    return 0
  fi

  local token
  token="$(python3 - <<'PY'
import json, os, pathlib

config_home = os.environ.get("XDG_CONFIG_HOME")
if config_home:
    canonical = pathlib.Path(config_home) / "ato" / "credentials.toml"
else:
    canonical = pathlib.Path.home() / ".config" / "ato" / "credentials.toml"
legacy = pathlib.Path.home() / ".ato" / "credentials.json"

def read_token(path):
    if not path.exists():
        return ""
    try:
        if path.suffix == ".toml":
            import tomllib
            data = tomllib.loads(path.read_text())
        else:
            data = json.loads(path.read_text())
    except Exception:
        return ""
    return (data.get("session_token") or "").strip()

print(read_token(canonical) or read_token(legacy))
PY
)"
  if [ -n "${token}" ]; then
    export ATO_TOKEN="${token}"
    return 0
  fi

  return 1
}

run_publish_auto_submit_check() {
  local log_file="$1"
  local registry_url="${E2E_REGISTRY_URL:-https://staging.api.ato.run}"
  local repo_url="${E2E_PUBLISH_REPO_URL:-}"

  if [ "${E2E_SKIP_PUBLISH:-0}" = "1" ]; then
    log_warn "Publish checks skipped by E2E_SKIP_PUBLISH=1"
    return 0
  fi

  if [ -z "${repo_url}" ]; then
    log_warn "Publish checks skipped: set E2E_PUBLISH_REPO_URL to enable"
    return 0
  fi

  if ! resolve_session_token; then
    log_warn "No session token found; skipping publish checks"
    return 0
  fi

  local normalized_repo_url="${repo_url%/}"
  local capsule_publisher
  local capsule_slug
  capsule_publisher="$(printf '%s' "${normalized_repo_url}" | sed -E 's#^(git@[^:]+:|https?://[^/]+/)([^/]+)/([^/]+?)(\\.git)?$#\\2#')"
  capsule_slug="$(basename "${repo_url}")"
  capsule_slug="${capsule_slug%.git}"

  if "${ATO_CLI}" publish "${repo_url}" --apply-playground --registry "${registry_url}" --json >"${log_file}" 2>&1; then
    log_info "publish --apply-playground succeeded"
  else
    if grep -q 'source_exists' "${log_file}"; then
      if [ "${E2E_ALLOW_SOURCE_EXISTS:-0}" = "1" ]; then
        log_warn "publish returned source_exists and is allowed by E2E_ALLOW_SOURCE_EXISTS=1"
        curl -sS "${registry_url}/v1/manifest/capsules/by/${capsule_publisher}/${capsule_slug}" > "${E2E_WORK_DIR}/capsule_detail_existing.json"
        python3 - <<'PY' "${E2E_WORK_DIR}/capsule_detail_existing.json"
import json, sys, pathlib
p = pathlib.Path(sys.argv[1])
obj = json.loads(p.read_text())
status = (obj.get("latest_review_status") or "").lower()
if status != "submitted":
    print(f"existing source latest_review_status is not submitted: {status}")
    raise SystemExit(1)
print("existing source latest_review_status=submitted")
PY
        log_info "existing source submitted status verified"
        return 0
      fi
      log_error "publish failed: source already exists (set E2E_ALLOW_SOURCE_EXISTS=1 to allow)"
      cat "${log_file}"
      exit 1
    fi
    log_error "publish --apply-playground failed"
    cat "${log_file}"
    exit 1
  fi

  python3 - <<'PY' "${log_file}"
import json, sys, pathlib
p = pathlib.Path(sys.argv[1])
raw = p.read_text().strip()
if not raw:
    print("publish output is empty")
    raise SystemExit(1)
obj = json.loads(raw)
if obj.get("auto_submit_playground") is not True:
    print("auto_submit_playground is not true")
    raise SystemExit(1)
result = obj.get("auto_submit_result")
if result is not None:
    status = (result.get("review_status") or "").lower()
    if status != "submitted":
        print(f"unexpected review_status: {status}")
        raise SystemExit(1)
print("publish assertions passed")
PY
  log_info "publish response assertions passed"

  curl -sS "${registry_url}/v1/manifest/capsules/by/${capsule_publisher}/${capsule_slug}" > "${E2E_WORK_DIR}/capsule_detail.json"
  python3 - <<'PY' "${E2E_WORK_DIR}/capsule_detail.json"
import json, sys, pathlib
p = pathlib.Path(sys.argv[1])
obj = json.loads(p.read_text())
status = (obj.get("latest_review_status") or "").lower()
if status != "submitted":
    print(f"latest_review_status is not submitted: {status}")
    raise SystemExit(1)
print("latest_review_status=submitted")
PY
  log_info "submitted status verified via capsule detail"
}

echo "=========================================="
echo "E2E: Zero Config & Auto Submit"
echo "=========================================="

ensure_cli

PY_DIR="${E2E_WORK_DIR}/python-no-manifest"
mkdir -p "${PY_DIR}"
cat >"${PY_DIR}/main.py" <<'PY'
print("hello zero config")
PY
run_zero_config_build_case "python" "${PY_DIR}" "${E2E_WORK_DIR}/python_build.log"

if [ "${E2E_RUN_NODE_CASE:-1}" = "1" ]; then
  NODE_DIR="${E2E_WORK_DIR}/node-no-manifest"
  mkdir -p "${NODE_DIR}"
  cat >"${NODE_DIR}/package.json" <<'JSON'
{
  "name": "zero-config-node",
  "version": "0.1.0",
  "type": "module"
}
JSON
  cat >"${NODE_DIR}/index.js" <<'JS'
console.log("hello zero config node");
JS
  run_zero_config_build_case "node" "${NODE_DIR}" "${E2E_WORK_DIR}/node_build.log"
else
  log_warn "Node case skipped by E2E_RUN_NODE_CASE=0"
fi

run_publish_auto_submit_check "${E2E_WORK_DIR}/publish.log"

log_info "Zero Config & Auto Submit E2E completed"
