#!/usr/bin/env bash
# AODD Recipe Regression Harness
#
# Run a selected recipe with clean ATO_HOME, verify endpoint and ato ps,
# stop all, verify Podman cleanup, write receipt.
#
# Usage:
#   scripts/aodd-recipes.sh samples/recipes/memos
#   scripts/aodd-recipes.sh --list docs/recipes/validated-recipes.txt
#   scripts/aodd-recipes.sh --help
#
# Receipt output:
#   .tmp/aodd-receipts/catalog-regression/<recipe>.yaml
set -euo pipefail

SELF="$(basename "$0")"
NOW="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
OS_ARCH="$(uname -s)/$(uname -m)"
RECEIPT_DIR=".tmp/aodd-receipts/catalog-regression"
HOST_STATE_DIR="${HOME}/.ato-aodd-regression"

usage() {
  sed -n '2,12p;13q' "$0"
  exit 0
}

info()  { printf "[%s] %s\n" "$SELF" "$*"; }
die()   { info "FATAL: $*"; exit 1; }
pass()  { printf "  ✅ %s\n" "$*"; }
fail()  { printf "  ❌ %s\n" "$*"; }
warn()  { printf "  ⚠️  %s\n" "$*"; }

cleanup() {
  local rc=$?
  if [ -n "${ATO_SESSION_ID:-}" ]; then
    info "Cleanup: stopping session $ATO_SESSION_ID"
    ato stop --id "$ATO_SESSION_ID" --force 2>/dev/null || true
  fi
  ato stop --all --force 2>/dev/null || true
  # Podman cleanup check
  if [ -n "${CONTAINER_FILTER:-}" ]; then
    local left
    left="$(podman ps --filter "name=$CONTAINER_FILTER" -q 2>/dev/null || true)"
    if [ -n "$left" ]; then
      warn "Leftover containers: $left"
    fi
  fi
  if [ -n "${NETWORK_FILTER:-}" ]; then
    local net_left
    net_left="$(podman network ls --filter "name=$NETWORK_FILTER" -q 2>/dev/null || true)"
    if [ -n "$net_left" ]; then
      warn "Leftover networks: $net_left"
    fi
  fi
  exit $rc
}

run_single() {
  local recipe_path="$1"
  local recipe_name
  recipe_name="$(basename "$recipe_path")"
  local ato_home
  ato_home="$(mktemp -d "$HOST_STATE_DIR/$recipe_name.XXXXXX")"
  export ATO_HOME="$ato_home"

  local state_dirs=()
  local state_flags=()

  # Parse recipe for state definitions
  # Extract explicit state names from TOML
  while IFS='|' read -r state_name state_durability; do
    if [ -n "$state_name" ]; then
      local state_dir="$ato_home/state-$state_name"
      mkdir -p "$state_dir"
      state_dirs+=("$state_name")
      state_flags+=("--state" "$state_name=$state_dir")
    fi
  done < <(toml_get_states "$recipe_path")

  local start_time end_time elapsed endpoint http_code
  local result_status="pass"
  local failures=()
  local image_digests="{}"

  info "=== AODD: $recipe_name ==="
  info "Recipe: $recipe_path"
  info "ATO_HOME: $ATO_HOME"
  info "OS/Arch: $OS_ARCH"

  CONTAINER_FILTER="ato-${recipe_name}-"
  NETWORK_FILTER="ato-${recipe_name}-"

  trap cleanup EXIT

  # Step 1: ato run
  start_time="$(date +%s)"
  info "Starting: ato run $recipe_path ${state_flags[*]}"
  if ato run "$recipe_path" "${state_flags[@]}" 2>&1; then
    end_time="$(date +%s)"
    elapsed=$((end_time - start_time))
    pass "Startup completed in ${elapsed}s"
  else
    end_time="$(date +%s)"
    elapsed=$((end_time - start_time))
    result_status="fail"
    failures+=("ato run failed after ${elapsed}s")
    warn "ato run failed"
  fi

  # Step 2: ato ps
  local ps_json=""
  if [ "$result_status" != "fail" ]; then
    info "Checking: ato ps --all --json"
    ps_json="$(ato ps --all --json 2>/dev/null || true)"
    if echo "$ps_json" | python3 -c "import sys,json; data=json.load(sys.stdin); assert isinstance(data, list) and len(data) > 0" 2>/dev/null; then
      endpoint="$(echo "$ps_json" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d[0].get('main_endpoint',''))" 2>/dev/null || true)"
      pass "ato ps shows session"
    else
      result_status="fail"
      endpoint=""
      failures+=("ato ps returned no session")
      fail "ato ps shows no session"
    fi
  fi

  # Step 3: endpoint check
  if [ -n "$endpoint" ]; then
    info "Checking endpoint: $endpoint"
    if http_code="$(curl -fsS -o /dev/null -w '%{http_code}' --max-time 15 "$endpoint" 2>/dev/null || true)"; then
      pass "Endpoint HTTP $http_code"
    else
      # curl returns 000 on connection error
      http_code="${http_code:-000}"
      warn "Endpoint returned HTTP $http_code"
      if [ "$result_status" != "fail" ]; then
        result_status="partial"
        failures+=("endpoint HTTP $http_code")
      fi
    fi
  fi

  # Step 4: ato stop --all
  info "Stopping: ato stop --all --force"
  if ato stop --all --force 2>&1; then
    pass "ato stop --all succeeded"
  else
    warn "ato stop --all encountered issues"
    if [ "$result_status" = "pass" ]; then
      result_status="partial"
    fi
    failures+=("ato stop --all had issues")
  fi

  # Step 5: Verify Podman cleanup
  local container_left=""
  local network_left=""
  sleep 2
  container_left="$(podman ps -a --filter "name=$CONTAINER_FILTER" -q 2>/dev/null || true)"
  network_left="$(podman network ls --filter "name=$NETWORK_FILTER" -q 2>/dev/null || true)"
  if [ -z "$container_left" ]; then
    pass "Containers cleaned up"
  else
    warn "Containers still present: $container_left"
    failures+=("containers not cleaned")
    result_status="partial"
  fi
  if [ -z "$network_left" ]; then
    pass "Networks cleaned up"
  else
    warn "Networks still present: $network_left"
    failures+=("networks not cleaned")
    result_status="partial"
  fi

  # Step 6: Write receipt
  mkdir -p "$RECEIPT_DIR"
  local receipt="$RECEIPT_DIR/$recipe_name.yaml"
  {
    echo "# AODD receipt — $recipe_name"
    echo "# Generated: $NOW"
    echo "recipe_name: \"$recipe_name\""
    echo "recipe_path: \"$recipe_path\""
    echo "ato_home: \"$ATO_HOME\""
    echo "os_arch: \"$OS_ARCH\""
    echo "command: \"ato run $recipe_path ${state_flags[*]}\""
    echo "startup_time_seconds: $elapsed"
    echo "endpoint: \"${endpoint:-}\""
    echo "status_code: \"${http_code:-}\""
    echo "cleanup_result: \"$([ -z "$container_left" ] && echo "clean" || echo "leaked: $container_left")\""
    echo "image_digest: \"${image_digests}\""
    echo "status: \"$result_status\""
    if [ ${#failures[@]} -gt 0 ]; then
      echo "failures:"
      for f in "${failures[@]}"; do
        echo "  - \"$f\""
      done
    fi
  } > "$receipt"
  pass "Receipt written: $receipt"

  info "=== Result: $result_status ==="
}

toml_get_states() {
  local recipe_path="$1"
  local capsule_toml="$recipe_path/capsule.toml"
  if [ ! -f "$capsule_toml" ]; then
    warn "No capsule.toml found at $capsule_toml"
    return
  fi
  # Extract state names with durability = "persistent"
  python3 -c "
import tomllib
with open('$capsule_toml', 'rb') as f:
    data = tomllib.load(f)
states = data.get('state', {})
for name, cfg in states.items():
    dur = cfg.get('durability', '')
    if cfg.get('attach', '') == 'explicit':
        print(f'{name}|{dur}')
" 2>/dev/null || true
}

run_list() {
  local list_file="$1"
  if [ ! -f "$list_file" ]; then
    die "List file not found: $list_file"
  fi
  while IFS= read -r line; do
    # Skip empty lines and comments
    case "$line" in
      ''|'#'*) continue ;;
    esac
    run_single "$line"
    echo ""
  done < "$list_file"
}

# ── Main ──

case "${1:-}" in
  --help|-h)
    usage
    ;;
  --list)
    [ -n "${2:-}" ] || die "--list requires a file argument"
    run_list "$2"
    ;;
  "")
    usage
    ;;
  *)
    run_single "$1"
    ;;
esac
