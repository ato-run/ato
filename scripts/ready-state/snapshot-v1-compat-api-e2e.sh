#!/usr/bin/env bash
# Snapshot v1 Compatibility Suite — API E2E (step 5 of the v1 compat order).
#
# Drives the registry API end-to-end for every fixture in
# tools/snapshot-builder/fixtures/compat/: enqueue → poll to terminal →
# assert the seal-side half of each fixture's expected.json (the eligibility
# half is enforced KVM-free by crates/snapshot/tests/compat_fixtures.rs).
#
# Prereqs:
#   - fixtures merged + seeded:  ato-api scripts/staging/seed-snapshot-compat-recipes.ts
#   - a snapshot builder claiming jobs against the same API (KVM host)
#   - ATO_SESSION_COOKIE: an authenticated account session cookie header value
#     (e.g. "better-auth.session_token=...") — enqueue/status are owner-scoped.
#
# Usage:
#   ATO_SESSION_COOKIE='...' scripts/ready-state/snapshot-v1-compat-api-e2e.sh \
#     [--api https://staging.api.ato.run] [--fixtures <dir>] [--out <dir>] \
#     [--timeout-min 45] [--only <name>[,<name>...]] [--plan]
#
# planted-builder-token is SKIPped unless --include-fault-injection is passed:
# its live-credential leak cannot ride pinned public source (see the fixture
# README) — the flag documents that the builder-host fault hook is armed.
set -euo pipefail

API="https://staging.api.ato.run"
FIXTURES="tools/snapshot-builder/fixtures/compat"
OUT=""
TIMEOUT_MIN=45
ONLY=""
PLAN=0
FAULT_INJECTION=0
ANCHOR_NAME="real-store-receipt-to-csv"
ANCHOR_CAPSULE_ID="${ATO_COMPAT_ANCHOR_CAPSULE_ID:-01KW10AKZ8GG7CANC6EVKWAYQ7}"
ANCHOR_TARGET_LABEL="${ATO_COMPAT_ANCHOR_TARGET_LABEL:-main}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --api) API="$2"; shift 2 ;;
    --fixtures) FIXTURES="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --timeout-min) TIMEOUT_MIN="$2"; shift 2 ;;
    --only) ONLY="$2"; shift 2 ;;
    --plan) PLAN=1; shift ;;
    --include-fault-injection) FAULT_INJECTION=1; shift ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

command -v jq >/dev/null || { echo "jq required" >&2; exit 2; }
[[ -d "$FIXTURES" ]] || { echo "fixtures dir not found: $FIXTURES (run from the ato repo root)" >&2; exit 2; }
if [[ $PLAN -eq 0 && -z "${ATO_SESSION_COOKIE:-}" ]]; then
  echo "ATO_SESSION_COOKIE required (owner-scoped enqueue/status)" >&2; exit 2
fi

OUT="${OUT:-benchmarks/ready-state/compat/$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p "$OUT"

curl_api() { # method path [json-body]
  local method="$1" path="$2" body="${3:-}"
  if [[ -n "$body" ]]; then
    curl -sS -X "$method" "$API$path" -H "Cookie: $ATO_SESSION_COOKIE" \
      -H 'Accept: application/json' -H 'Content-Type: application/json' --data "$body"
  else
    curl -sS -X "$method" "$API$path" -H "Cookie: $ATO_SESSION_COOKIE" -H 'Accept: application/json'
  fi
}

capsule_id_for() { # fixture name → capsule id
  local name="$1"
  if [[ "$name" == "$ANCHOR_NAME" ]]; then echo "$ANCHOR_CAPSULE_ID"; else echo "cap_compat_${name//-/_}"; fi
}

# ── collect fixtures ─────────────────────────────────────────────────────────
NAMES=()
for d in "$FIXTURES"/*/; do
  n="$(basename "$d")"
  [[ -f "$d/expected.json" ]] || continue
  if [[ -n "$ONLY" && ",$ONLY," != *",$n,"* ]]; then continue; fi
  NAMES+=("$n")
done
[[ ${#NAMES[@]} -gt 0 ]] || { echo "no fixtures matched" >&2; exit 2; }

if [[ $PLAN -eq 1 ]]; then
  for n in "${NAMES[@]}"; do
    exp="$FIXTURES/$n/expected.json"
    printf '%-28s capsule=%-40s seal=%-7s stage=%s\n' "$n" "$(capsule_id_for "$n")" \
      "$(jq -r .seal "$exp")" "$(jq -r '.seal_failure_stage // "-"' "$exp")"
  done
  exit 0
fi

# ── enqueue all first (builder drains the queue; polling overlaps builds) ────
declare -A JOB EXPECTED_SEAL EXPECTED_STAGE VERDICT DETAIL
for n in "${NAMES[@]}"; do
  exp="$FIXTURES/$n/expected.json"
  EXPECTED_SEAL[$n]="$(jq -r .seal "$exp")"
  EXPECTED_STAGE[$n]="$(jq -r '.seal_failure_stage // ""' "$exp")"
  if [[ "$n" == "planted-builder-token" && $FAULT_INJECTION -eq 0 ]]; then
    VERDICT[$n]="SKIP"; DETAIL[$n]="needs --include-fault-injection (builder-host hook; see fixture README)"
    continue
  fi
  cap="$(capsule_id_for "$n")"
  body='{}'
  [[ "$n" == "$ANCHOR_NAME" ]] && body="{\"target_label\":\"$ANCHOR_TARGET_LABEL\"}"
  resp="$(curl_api POST "/v1/capsules/$cap/snapshot-jobs" "$body")"
  job="$(jq -r '.job_id // empty' <<<"$resp")"
  if [[ -z "$job" ]]; then
    VERDICT[$n]="FAIL"; DETAIL[$n]="enqueue refused: $(jq -c . <<<"$resp" 2>/dev/null || echo "$resp")"
    continue
  fi
  JOB[$n]="$job"
  echo "enqueued $n → $job (capsule $cap)"
done

# ── poll to terminal ─────────────────────────────────────────────────────────
deadline=$(( $(date +%s) + TIMEOUT_MIN * 60 ))
pending=()
for n in "${NAMES[@]}"; do [[ -n "${JOB[$n]:-}" ]] && pending+=("$n"); done
while [[ ${#pending[@]} -gt 0 && $(date +%s) -lt $deadline ]]; do
  sleep 10
  next=()
  for n in "${pending[@]}"; do
    j="$(curl_api GET "/v1/snapshot-jobs/${JOB[$n]}")"
    status="$(jq -r '.status // "unknown"' <<<"$j")"
    case "$status" in
      sealed|failed) echo "$n → $status"; jq -c . <<<"$j" > "$OUT/job-$n.json" ;;
      *) next+=("$n") ;;
    esac
  done
  pending=("${next[@]:-}")
  [[ ${#pending[@]} -eq 1 && -z "${pending[0]}" ]] && pending=()
done
for n in "${pending[@]:-}"; do
  [[ -n "$n" ]] && { VERDICT[$n]="FAIL"; DETAIL[$n]="not terminal after ${TIMEOUT_MIN}min"; }
done

# ── assert ───────────────────────────────────────────────────────────────────
for n in "${NAMES[@]}"; do
  [[ -n "${VERDICT[$n]:-}" ]] && continue
  j="$(cat "$OUT/job-$n.json")"
  status="$(jq -r .status <<<"$j")"
  exp="$FIXTURES/$n/expected.json"
  if [[ "$status" != "${EXPECTED_SEAL[$n]}" ]]; then
    VERDICT[$n]="FAIL"; DETAIL[$n]="expected ${EXPECTED_SEAL[$n]}, got $status ($(jq -r '.error_summary // ""' <<<"$j"))"
    continue
  fi
  if [[ "$status" == "failed" ]]; then
    stage="$(jq -r '.receipt.failure_stage // ""' <<<"$j")"
    if [[ "$stage" != "${EXPECTED_STAGE[$n]}" ]]; then
      VERDICT[$n]="FAIL"; DETAIL[$n]="expected stage ${EXPECTED_STAGE[$n]}, got ${stage:-<none>}"
      continue
    fi
    needle="$(jq -r '.eligibility_reason_contains // ""' "$exp")"
    summary="$(jq -r '.error_summary // ""' <<<"$j")"
    if [[ -n "$needle" && "$summary" != *"$needle"* ]]; then
      VERDICT[$n]="FAIL"; DETAIL[$n]="error_summary lacks the documented reason ($needle): $summary"
      continue
    fi
    VERDICT[$n]="PASS"; DETAIL[$n]="failed at $stage as contracted"
  else
    cap="$(capsule_id_for "$n")"
    latest="$(curl_api GET "/v1/capsules/$cap/snapshots/latest")"
    if [[ "$(jq -r '.public_run_eligible // false' <<<"$latest")" != "true" ]]; then
      VERDICT[$n]="FAIL"; DETAIL[$n]="sealed but snapshots/latest not public_run_eligible: $(jq -c . <<<"$latest")"
      continue
    fi
    echo "$latest" > "$OUT/latest-$n.json"
    want_synth="$(jq -r '.probe_synthesized // empty' "$exp")"
    got_synth="$(jq -r '.receipt.artifact.synthesized_probe // .receipt.synthesized_probe // empty' <<<"$j")"
    if [[ -n "$want_synth" && -n "$got_synth" && "$want_synth" != "$got_synth" ]]; then
      VERDICT[$n]="FAIL"; DETAIL[$n]="receipt synthesized_probe=$got_synth, expected $want_synth"
      continue
    fi
    if [[ "$(jq -r '.advisory_pem_expected // false' "$exp")" == "true" ]] && ! grep -qi "pem" <<<"$j"; then
      VERDICT[$n]="FAIL"; DETAIL[$n]="sealed, but the receipt carries no PEM advisory (guarantee: advisory fires without gating)"
      continue
    fi
    VERDICT[$n]="PASS"; DETAIL[$n]="sealed, registry-visible"
  fi
done

# ── report ───────────────────────────────────────────────────────────────────
pass=0; fail=0; skip=0
{
  echo "# Snapshot v1 Compatibility Suite — API E2E"
  echo
  echo "api: $API  fixtures: $FIXTURES  at: $(date -u +%FT%TZ)"
  echo
  printf '| %-28s | %-7s | %s |\n' fixture verdict detail
  printf '|%s|%s|%s|\n' "$(printf -- '-%.0s' {1..30})" "$(printf -- '-%.0s' {1..9})" "$(printf -- '-%.0s' {1..40})"
  for n in "${NAMES[@]}"; do
    printf '| %-28s | %-7s | %s |\n' "$n" "${VERDICT[$n]}" "${DETAIL[$n]}"
    case "${VERDICT[$n]}" in PASS) ((pass+=1));; FAIL) ((fail+=1));; SKIP) ((skip+=1));; esac
  done
  echo
  echo "PASS=$pass FAIL=$fail SKIP=$skip"
} | tee "$OUT/summary.md"

for n in "${NAMES[@]}"; do
  jq -n --arg name "$n" --arg verdict "${VERDICT[$n]}" --arg detail "${DETAIL[$n]}" \
    '{name:$name, verdict:$verdict, detail:$detail}'
done | jq -s . > "$OUT/results.json"
echo "artifacts: $OUT"
[[ $fail -eq 0 ]]
