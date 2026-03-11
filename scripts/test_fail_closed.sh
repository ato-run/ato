#!/usr/bin/env bash
set -euo pipefail

CLI_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ ! -f "${CLI_DIR}/Cargo.toml" ]]; then
  echo "[ERROR] Cargo.toml が見つかりません: ${CLI_DIR}" >&2
  exit 1
fi

TEST_BIN=(cargo test -p ato-cli --test fail_closed_test)
TEST_FLAGS=(-- --ignored --nocapture)

SCENARIOS=(
  "test_5_non_interactive_missing_consent_denied"
  "test_5_yes_flag_does_not_bypass_missing_consent"
  "test_14_reconsent_required_on_policy_change"
  "test_15_npm_package_lock_fallback_success"
  "test_16_airgap_offline_execution_success"
  "test_17_tier2_native_fs_isolation_enforced"
  "test_18_from_skill_missing_consent_denied"
  "test_19_self_healing_loop_recovers_from_policy_violation"
  "test_2_deno_lock_missing_fail_closed"
  "test_3_native_python_uv_lock_missing_fail_closed"
)

echo "[INFO] fail-closed ${#SCENARIOS[@]}シナリオを実行します"
echo "[INFO] repo=${CLI_DIR}"

declare -a passed=()
declare -a failed=()

cd "${CLI_DIR}"
for scenario in "${SCENARIOS[@]}"; do
  echo
  echo "== ${scenario} =="
  if "${TEST_BIN[@]}" "${scenario}" "${TEST_FLAGS[@]}"; then
    passed+=("${scenario}")
  else
    failed+=("${scenario}")
  fi
done

echo
if [[ ${#failed[@]} -eq 0 ]]; then
  echo "[PASS] 全${#passed[@]}シナリオ成功"
  exit 0
fi

echo "[FAIL] 失敗: ${#failed[@]} / 成功: ${#passed[@]}"
printf '  - %s\n' "${failed[@]}"
exit 1
